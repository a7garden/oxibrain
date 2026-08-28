//! oxibrain-client: thin async client for consuming apps (DESIGN §12.1, §15).
//!
//! Daemonless transport (two-plane design): the client spawns or attaches to a
//! caller-owned `oxibrain serve --stdio` child and speaks newline-delimited
//! JSON-RPC 2.0 over its piped stdio — the mirror of `oxibrain_mcp::run_session`.
//! Each tool call sends a `tools/call` request and reads one response line.
//! There is no socket discovery and no shared daemon: the parent owns the
//! child, and dropping the client tears it down (`kill_on_drop`).
//!
//! Token authentication: `spawn_local_with_token` sends an `auth` request as
//! the first message. The server resolves the token to a `Scope` that gates
//! every subsequent call (DESIGN §11.2).
//!
//! Capability handshake: `handshake` performs the transport-level negotiation
//! described in `doc/spec/oxi-foundation-v1.md` §8. It rides the same stdio
//! channel; the MCP tool list stays at fifteen.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod foundation_package;

pub mod protocol;

pub use foundation_package::{
    AbstractRequirement, FoundationPackage, PackageError, PackageManifest, PackagePersona,
    PackagesLock, PayloadLocation, TrustState, foundation_home, load_package_manifest,
    load_packages_lock, manifest_path, parse_package_manifest, parse_packages_lock,
    select_package_for_target,
};
pub use protocol::{
    BrainCapabilities, BrainProtocolVersion, ClientHello, ClientOperation, HandshakeError,
    ServerInfo, default_client_hello, parse_handshake_error,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// A locally spawned `oxibrain serve --stdio` child the client talks to.
///
/// The executable is typically the installed `oxibrain` binary (or a path
/// built from this workspace). `dir` is the brain store directory the child
/// serves; it is passed as `--dir` so the child never falls back to the
/// `$HOME` default (two-plane invariant 15: no ambient store access).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProcessEndpoint {
    pub executable: PathBuf,
    pub dir: PathBuf,
}

impl LocalProcessEndpoint {
    /// Build an endpoint for `executable` serving the store at `dir`.
    pub fn new(executable: impl Into<PathBuf>, dir: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            dir: dir.into(),
        }
    }
}

/// A client attached to an oxibrain server over newline-delimited JSON-RPC.
///
/// Methods mirror the MCP tool surface (DESIGN §12.2) plus the native
/// JSON-RPC methods (`document_history`, `pending_stats`, `extract_uncached`)
/// that stay outside the fifteen-tool MCP cap.
pub struct BrainClient {
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    /// The spawned child, if this client owns one. Held so `kill_on_drop`
    /// keeps applying for the client's lifetime; dropping the client reaps it.
    child: Option<tokio::process::Child>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for BrainClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrainClient")
            .field("owns_child", &self.child.is_some())
            .field("next_id", &self.next_id)
            .finish()
    }
}

/// A space as enumerated by [`BrainClient::list_spaces`] — client-owned DTO, no engine types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceSummary {
    pub id: String,
    pub name: String,
    #[serde(rename = "created_at")]
    pub created_at_ms: i64,
    pub episode_count: i64,
    pub entity_count: i64,
}

/// One historical revision of a document, as returned by
/// [`BrainClient::document_history`]. `content` is the committed text
/// (lossy-UTF8 when the blob is not valid UTF-8); `committed_at_ms` is the
/// commit time in milliseconds since the epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRevisionDto {
    pub revision: String,
    pub committed_at_ms: i64,
    pub content: String,
}

/// A documents-plane hit in [`SearchResponseDto::documents`] — client-owned
/// mirror of the facade's `DocumentHit` (millis on the wire).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentHitDto {
    pub document_id: String,
    pub root: String,
    pub locator: String,
    pub revision: String,
    pub ordinal: u32,
    pub text: String,
    /// Millis since epoch; the wire key is `modified_at` (engine shape).
    #[serde(rename = "modified_at")]
    pub modified_at_ms: i64,
    pub score: f64,
}

/// Freshness summary of the documents plane during a search — client-owned
/// mirror of the facade's `DocumentFreshness`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentFreshnessDto {
    pub reconciled_roots: Vec<String>,
    /// `(alias, reason)` pairs; serializes as two-element arrays.
    pub skipped_roots: Vec<(String, String)>,
    pub skipped_files: u64,
    pub stale_after_retry: Vec<String>,
    pub dense_coverage: Option<f64>,
}

/// The search envelope: memory-plane hits (raw engine shape, kept as JSON),
/// documents-plane hits, and the reconcile freshness report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponseDto {
    pub memory: Vec<Value>,
    pub documents: Vec<DocumentHitDto>,
    pub freshness: DocumentFreshnessDto,
}

/// Memory-plane extraction backlog, as returned by
/// [`BrainClient::pending_stats`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingStatsDto {
    pub count: u64,
    pub oldest_seq: Option<u64>,
}

impl BrainClient {
    /// Attach to an existing read/write JSON-RPC stream (newline-delimited).
    ///
    /// In-process counterpart of [`BrainClient::spawn_local`]: tests and
    /// embedders can drive a `run_session` server over a duplex pipe without
    /// spawning a process.
    pub fn from_io(
        reader: impl AsyncRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
    ) -> Self {
        Self {
            writer: Box::new(writer),
            reader: BufReader::new(Box::new(reader)),
            child: None,
            next_id: AtomicU64::new(1),
        }
    }

    /// Spawn `executable serve --stdio --dir <dir>` and talk to it over piped
    /// stdio. The child is killed when the client is dropped (`kill_on_drop`);
    /// nothing else — no socket, no discovery, no ambient store access.
    pub async fn spawn_local(endpoint: LocalProcessEndpoint) -> Result<Self> {
        let child = Self::spawn_child(&endpoint).await?;
        let (child, reader, writer) = Self::take_pipes(&endpoint, child)?;
        Ok(Self {
            writer: Box::new(writer),
            reader: BufReader::new(Box::new(reader)),
            child: Some(child),
            next_id: AtomicU64::new(1),
        })
    }

    /// Like [`BrainClient::spawn_local`], but authenticates first: the child
    /// expects an `auth` request as its first message and resolves the token
    /// to a `Scope` gating every subsequent call (DESIGN §11.2).
    pub async fn spawn_local_with_token(
        endpoint: LocalProcessEndpoint,
        token: &str,
    ) -> Result<Self> {
        let mut client = Self::spawn_local(endpoint).await?;
        client.auth(token).await?;
        Ok(client)
    }

    async fn spawn_child(endpoint: &LocalProcessEndpoint) -> Result<tokio::process::Child> {
        let mut cmd = tokio::process::Command::new(&endpoint.executable);
        // Arg order is part of the daemonless contract: `admin serve --stdio
        // --dir` (v2.13: serve lives under the admin namespace — ADR-012).
        cmd.arg("admin")
            .arg("serve")
            .arg("--stdio")
            .arg("--dir")
            .arg(&endpoint.dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        cmd.spawn()
            .with_context(|| format!("spawn {}", endpoint.executable.display()))
    }

    /// Take the child's stdio pipes. The child is moved back to the caller
    /// together with its pipes so `kill_on_drop` keeps applying.
    fn take_pipes(
        endpoint: &LocalProcessEndpoint,
        mut child: tokio::process::Child,
    ) -> Result<(
        tokio::process::Child,
        tokio::process::ChildStdout,
        tokio::process::ChildStdin,
    )> {
        let stdin = child
            .stdin
            .take()
            .with_context(|| format!("child stdin missing: {}", endpoint.executable.display()))?;
        let stdout = child
            .stdout
            .take()
            .with_context(|| format!("child stdout missing: {}", endpoint.executable.display()))?;
        Ok((child, stdout, stdin))
    }

    /// Send the `auth` request over the current stream. Used by
    /// [`BrainClient::spawn_local_with_token`]; exposed for callers that
    /// attach with [`BrainClient::from_io`] to a gated session.
    pub async fn auth(&mut self, token: &str) -> Result<()> {
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "auth",
            "params": { "token": token }
        });
        self.send(&req).await?;
        let resp = self.recv().await?;
        if let Some(err) = resp.get("error") {
            bail!(
                "authentication failed: {}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            );
        }
        Ok(())
    }

    /// Negotiate capabilities with the server.
    ///
    /// Sends a `handshake` JSON-RPC request carrying the supplied
    /// [`ClientHello`] and parses the resulting [`ServerInfo`] into
    /// [`BrainCapabilities`]. On an incompatible-version error the server
    /// returns a JSON-RPC error with a typed [`HandshakeError`] in `data`;
    /// the client surfaces that error as a typed `Err`.
    pub async fn handshake(&mut self, hello: ClientHello) -> Result<BrainCapabilities> {
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": protocol::HANDSHAKE_METHOD,
            "params": hello,
        });
        self.send(&req).await?;
        let resp = self.recv().await?;
        if let Some(err) = resp.get("error") {
            if let Some(typed) = parse_handshake_error(err) {
                return Err(typed.into());
            }
            bail!(
                "handshake failed: {}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
            );
        }
        let result = resp
            .get("result")
            .context("missing result in handshake response")?;
        let info: ServerInfo = serde_json::from_value(result.clone())
            .context("parse ServerInfo from handshake result")?;
        Ok(info.into())
    }

    /// The Foundation bring-up sequence over a spawned child: optional `auth`
    /// first, then `handshake`. Returns the negotiated capabilities.
    pub async fn bring_up(
        &mut self,
        token: Option<&str>,
        hello: ClientHello,
    ) -> Result<BrainCapabilities> {
        if let Some(token) = token {
            self.auth(token).await?;
        }
        self.handshake(hello).await
    }

    // ── Low-level JSON-RPC ───────────────────────────────────────────────

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn send(&mut self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Value> {
        let mut buf = String::new();
        let n = self
            .reader
            .read_line(&mut buf)
            .await
            .context("read response")?;
        if n == 0 {
            bail!("server closed the connection");
        }
        serde_json::from_str(&buf).context("parse response")
    }

    /// Send a `tools/call` request and return the text content of the response.
    ///
    /// Protocol-level errors (scope denial, missing args) map to `Err`.
    /// Tool execution errors (isError) also map to `Err` with the tool's message.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<String> {
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        });
        self.send(&req).await?;
        let resp = self.recv().await?;

        if let Some(err) = resp.get("error") {
            bail!(
                "{}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
            );
        }

        let result = resp.get("result").context("missing result in response")?;

        if result.get("isError") == Some(&json!(true)) {
            let text = extract_text(result)?;
            bail!("{text}");
        }

        extract_text(result)
    }

    /// Like `call_tool` but returns the parsed JSON value.
    pub async fn call_tool_json(&mut self, name: &str, args: Value) -> Result<Value> {
        let text = self.call_tool(name, args).await?;
        serde_json::from_str(&text).context("parse tool result JSON")
    }

    // ── Convenience methods (MCP tool surface, DESIGN §12.2) ─────────────

    /// `search` over both planes (memory + documents). Returns the full
    /// envelope; see [`BrainClient::search_planes`] to restrict the planes.
    pub async fn search(
        &mut self,
        query: &str,
        space: &str,
        mode: &str,
        limit: usize,
    ) -> Result<SearchResponseDto> {
        self.call_search(query, space, mode, limit, None).await
    }

    /// `search` restricted to specific planes (`"memory"` and/or
    /// `"documents"`). Passing an empty slice asks the server for neither
    /// plane — a valid degenerate request returning empty lists.
    pub async fn search_planes(
        &mut self,
        query: &str,
        space: &str,
        mode: &str,
        limit: usize,
        planes: &[&str],
    ) -> Result<SearchResponseDto> {
        self.call_search(query, space, mode, limit, Some(planes))
            .await
    }

    async fn call_search(
        &mut self,
        query: &str,
        space: &str,
        mode: &str,
        limit: usize,
        planes: Option<&[&str]>,
    ) -> Result<SearchResponseDto> {
        let mut args = json!({
            "query": query,
            "space": space,
            "mode": mode,
            "limit": limit
        });
        if let Some(planes) = planes {
            args["planes"] = json!(planes);
        }
        let value = self.call_tool_json("search", args).await?;
        serde_json::from_value(value).context("parse search response")
    }

    /// `recall` — assemble_context for agent turns (Read cap).
    pub async fn recall(&mut self, query: &str, space: &str, token_budget: usize) -> Result<Value> {
        self.call_tool_json(
            "recall",
            json!({
                "query": query,
                "space": space,
                "token_budget": token_budget
            }),
        )
        .await
    }

    /// `get_entity` — entity beliefs and neighbors (Read cap).
    pub async fn get_entity(&mut self, entity_id: &str, space: &str) -> Result<Value> {
        self.call_tool_json(
            "get_entity",
            json!({ "entity_id": entity_id, "space": space }),
        )
        .await
    }

    /// `ingest` — ingest a note episode (Ingest cap). Returns the episode id.
    pub async fn ingest(
        &mut self,
        content: &str,
        space: &str,
        source_path: &str,
    ) -> Result<String> {
        self.call_tool(
            "ingest",
            json!({
                "content": content,
                "space": space,
                "source_path": source_path
            }),
        )
        .await
    }

    /// `declare` — deterministic entity/statement write, no LLM (Write cap).
    /// `declaration_json` is the serialized `Declaration` struct.
    pub async fn declare(&mut self, space: &str, declaration_json: &str) -> Result<String> {
        self.call_tool(
            "declare",
            json!({
                "space": space,
                "declaration_json": declaration_json
            }),
        )
        .await
    }

    /// `timeline` — belief intervals for an entity over a time range (Read cap).
    pub async fn timeline(
        &mut self,
        entity_id: &str,
        space: &str,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<Value> {
        let mut args = json!({ "entity_id": entity_id, "space": space });
        if let Some(from) = from {
            args["from"] = json!(from);
        }
        if let Some(to) = to {
            args["to"] = json!(to);
        }
        self.call_tool_json("timeline", args).await
    }

    /// `stats` — aggregate counts for a space (Read cap).
    pub async fn stats(&mut self, space: &str) -> Result<Value> {
        self.call_tool_json("stats", json!({ "space": space }))
            .await
    }

    /// `why` — provenance and confidence breakdown (Read cap).
    pub async fn why(&mut self, statement_id: &str, space: &str) -> Result<Value> {
        self.call_tool_json(
            "why",
            json!({ "statement_id": statement_id, "space": space }),
        )
        .await
    }

    /// `contradictions` — list contradicted statements (Read cap).
    pub async fn contradictions(&mut self, space: &str) -> Result<Value> {
        self.call_tool_json("contradictions", json!({ "space": space }))
            .await
    }

    /// `ping` — keepalive / latency check.
    pub async fn ping(&mut self) -> Result<()> {
        let id = self.alloc_id();
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": "ping" });
        self.send(&req).await?;
        let resp = self.recv().await?;
        if resp.get("result").is_none() {
            bail!("ping failed: no result");
        }
        Ok(())
    }

    // ── Native JSON-RPC methods (outside the fifteen-tool MCP cap) ────────

    /// Send a raw JSON-RPC request (non-tool method, e.g. `spaces/list`) and
    /// return the parsed `result`. Protocol errors map to `Err`.
    pub async fn call_rpc_json(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.alloc_id();
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send(&req).await?;
        let resp = self.recv().await?;
        if let Some(err) = resp.get("error") {
            bail!(
                "{}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
            );
        }
        resp.get("result")
            .cloned()
            .context("missing result in response")
    }

    /// Enumerate spaces the server exposes to this session (native RPC — not an
    /// MCP tool). Millis on the wire; convert to your own time type.
    pub async fn list_spaces(&mut self) -> Result<Vec<SpaceSummary>> {
        let v = self.call_rpc_json("spaces/list", json!({})).await?;
        serde_json::from_value(v.get("spaces").cloned().unwrap_or(json!([])))
            .context("parse spaces/list result")
    }

    /// Native method: commit history for one tracked document locator
    /// (git roots only), oldest first.
    pub async fn document_history(
        &mut self,
        space: &str,
        alias: &str,
        locator: &str,
        limit: usize,
    ) -> Result<Vec<DocumentRevisionDto>> {
        let v = self
            .call_rpc_json(
                "document_history",
                json!({ "space": space, "alias": alias, "locator": locator, "limit": limit }),
            )
            .await?;
        serde_json::from_value(v).context("parse document_history result")
    }

    /// Native method: memory-plane extraction backlog stats.
    pub async fn pending_stats(&mut self) -> Result<PendingStatsDto> {
        let v = self.call_rpc_json("pending_stats", json!({})).await?;
        serde_json::from_value(v).context("parse pending_stats result")
    }

    /// Native method: drain up to `limit` uncached memory-plane episodes
    /// through the server's configured extractor. Returns how many were
    /// extracted. Fails when the server has no LLM configured.
    pub async fn extract_uncached(&mut self, limit: usize) -> Result<u64> {
        let v = self
            .call_rpc_json("extract_uncached", json!({ "limit": limit }))
            .await?;
        v.get("extracted")
            .and_then(|n| n.as_u64())
            .context("parse extract_uncached result")
    }
}

/// Extract the text from an MCP result's first content block.
fn extract_text(result: &Value) -> Result<String> {
    let text = result
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .context("missing text in result")?;
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_parses_mcp_result() {
        let result = json!({
            "content": [{ "type": "text", "text": "hello world" }]
        });
        assert_eq!(extract_text(&result).unwrap(), "hello world");
    }

    #[test]
    fn extract_text_fails_without_content() {
        let result = json!({});
        assert!(extract_text(&result).is_err());
    }

    #[test]
    fn search_response_dto_parses_facade_envelope() {
        // Wire shape produced by the server's `search` tool (SearchResponse).
        let raw = json!({
            "memory": [{ "entity_id": "e1", "entity_surface": "Alice", "score": 1.0 }],
            "documents": [{
                "document_id": "d1",
                "root": "vault",
                "locator": "notes.md",
                "revision": "blake3:abc",
                "ordinal": 0,
                "text": "alpha content",
                "modified_at": 1700000000000i64,
                "score": 0.5
            }],
            "freshness": {
                "reconciled_roots": ["vault"],
                "skipped_roots": [["missing", "root not found"]],
                "skipped_files": 2,
                "stale_after_retry": [],
                "dense_coverage": null
            }
        });
        let dto: SearchResponseDto = serde_json::from_value(raw).unwrap();
        assert_eq!(dto.memory.len(), 1);
        assert_eq!(dto.documents.len(), 1);
        assert_eq!(dto.documents[0].modified_at_ms, 1700000000000i64);
        assert_eq!(
            dto.freshness.skipped_roots,
            vec![("missing".into(), "root not found".into())]
        );
        assert_eq!(dto.freshness.skipped_files, 2);
        assert!(dto.freshness.dense_coverage.is_none());
    }

    #[tokio::test]
    async fn spawn_local_missing_executable_fails_fast() {
        let endpoint = LocalProcessEndpoint::new(
            "/nonexistent/oxibrain-for-sure-missing",
            "/tmp/definitely-no-brain",
        );
        let start = std::time::Instant::now();
        let result = BrainClient::spawn_local(endpoint).await;
        let elapsed = start.elapsed();
        assert!(result.is_err(), "must error on missing executable");
        assert!(
            elapsed.as_secs() < 5,
            "took {elapsed:?}, expected fast failure"
        );
    }
}
