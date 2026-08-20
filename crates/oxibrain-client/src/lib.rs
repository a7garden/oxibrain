//! oxibrain-client: thin async client for consuming apps (DESIGN §12.1, §15).
//!
//! Speaks newline-delimited JSON-RPC 2.0 over a Unix-domain socket — the mirror
//! of `oxibrain_mcp::run_session`. Each tool call sends a `tools/call` request
//! and reads one response line. This makes `Brain` one trait in embedded and
//! daemon modes (P6): a consumer changes topology by switching from
//! `Brain::open` to `BrainClient::connect`.
//!
//! Token authentication: `connect_with_token` sends an `auth` handshake before
//! any tool call. The server resolves the token to a `Scope` that gates every
//! subsequent request (DESIGN §11.2).
//!
//! Capability handshake: `connect_default` / `connect_endpoint` perform the
//! transport-level `handshake` described in `doc/spec/oxi-foundation-v1.md`
//! §8. Discovery and capability negotiation ride on the same JSON-RPC socket;
//! the MCP tool list stays at fifteen.
//!
//! Platform: Unix-only (Unix-domain sockets). On non-Unix targets the crate
//! compiles but all methods return `UnsupportedPlatform`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod discovery;
pub mod foundation_package;

pub mod protocol;

pub use discovery::{BrainEndpoint, DiscoveryError, default_socket_path};
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
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Default client identity used by [`default_client_hello`] when the caller
/// does not supply one. Hosts (Oxicode, Oxios) override this to identify
/// themselves in `ClientHello.client_version`.
const DEFAULT_CLIENT_VERSION: &str =
    concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"));

/// A client connected to an oxibrain daemon over a Unix-domain socket.
///
/// Methods mirror the MCP tool surface (DESIGN §12.2). Each returns the raw
/// JSON text from the server's response — structured data the caller can parse.
#[derive(Debug)]
pub struct BrainClient {
    writer: tokio::net::unix::OwnedWriteHalf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    next_id: AtomicU64,
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

/// One `sync/run` pass outcome — client-owned DTO, no engine types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOutcome {
    pub new: Vec<String>,
    pub modified: Vec<String>,
    pub unchanged: Vec<String>,
}

impl BrainClient {
    /// Connect to an oxibrain daemon at a Unix socket path (trusted, no token).
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        #[cfg(unix)]
        {
            let stream = UnixStream::connect(path.as_ref())
                .await
                .with_context(|| format!("connect {}", path.as_ref().display()))?;
            let (reader, writer) = stream.into_split();
            Ok(Self {
                writer,
                reader: BufReader::new(reader),
                next_id: AtomicU64::new(1),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            bail!("Unix-domain sockets are not supported on this platform");
        }
    }

    /// Connect and authenticate with a token. The first message sent is an
    /// `auth` request; the server verifies the token and resolves a `Scope`
    /// that gates all subsequent tool calls.
    pub async fn connect_with_token(path: impl AsRef<Path>, token: &str) -> Result<Self> {
        let mut client = Self::connect(path).await?;
        let id = client.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "auth",
            "params": { "token": token }
        });
        client.send(&req).await?;
        let resp = client.recv().await?;
        if let Some(err) = resp.get("error") {
            bail!(
                "authentication failed: {}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            );
        }
        Ok(client)
    }

    /// Connect using the canonical Oxi Foundation default socket.
    ///
    /// Resolves the path via [`BrainEndpoint::default`] (which honors
    /// `$OXIBRAIN_SOCKET` and falls back to `$HOME/.oxi/brain/oxibrain.sock`)
    /// and then performs the transport-level `handshake` before returning.
    ///
    /// Errors fast when the daemon is absent (sub-second).
    pub async fn connect_default() -> Result<(Self, BrainCapabilities)> {
        let endpoint = BrainEndpoint::default()
            .with_context(|| "no default oxibrain socket: set $OXIBRAIN_SOCKET or $HOME")?;
        Self::connect_endpoint(&endpoint).await
    }

    /// Connect to a specific endpoint (validated path) and perform the
    /// `handshake`. Returns the negotiated [`BrainCapabilities`] alongside the
    /// client so the caller can branch on store format or server version.
    pub async fn connect_endpoint(endpoint: &BrainEndpoint) -> Result<(Self, BrainCapabilities)> {
        let mut client = Self::connect(endpoint.path()).await?;
        let caps = client
            .handshake(default_client_hello(DEFAULT_CLIENT_VERSION))
            .await?;
        Ok((client, caps))
    }

    /// Connect, optionally authenticate, and perform the handshake — the full
    /// Oxi Foundation bring-up sequence in one call.
    ///
    /// - On a token-protected socket, the `auth` request must come first; the
    ///   `Scope` resolved by the server gates every subsequent call.
    /// - The `handshake` request comes after optional auth and before any MCP
    ///   tool routing — matching the order in
    ///   `doc/spec/oxi-foundation-v1.md` §8.
    /// - On a trusted (no-token) socket, `token` may be `None`.
    pub async fn connect_endpoint_handshake(
        endpoint: &BrainEndpoint,
        token: Option<&str>,
    ) -> Result<(Self, BrainCapabilities)> {
        let mut client = if let Some(tok) = token {
            Self::connect_with_token(endpoint.path(), tok).await?
        } else {
            Self::connect(endpoint.path()).await?
        };
        let caps = client
            .handshake(default_client_hello(DEFAULT_CLIENT_VERSION))
            .await?;
        Ok((client, caps))
    }

    /// Negotiate capabilities with the daemon.
    ///
    /// Sends a `handshake` JSON-RPC request carrying the supplied
    /// [`ClientHello`] and parses the resulting [`ServerInfo`] into
    /// [`BrainCapabilities`]. On an incompatible-version error the daemon
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

    /// `search` — hybrid/lexical/semantic/graph/community query (Read cap).
    pub async fn search(
        &mut self,
        query: &str,
        space: &str,
        mode: &str,
        limit: usize,
    ) -> Result<Value> {
        self.call_tool_json(
            "search",
            json!({
                "query": query,
                "space": space,
                "mode": mode,
                "limit": limit
            }),
        )
        .await
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

    /// Enumerate spaces the daemon exposes to this session (native RPC — not an
    /// MCP tool). Millis on the wire; convert to your own time type.
    pub async fn list_spaces(&mut self) -> Result<Vec<SpaceSummary>> {
        let v = self.call_rpc_json("spaces/list", json!({})).await?;
        serde_json::from_value(v.get("spaces").cloned().unwrap_or(json!([])))
            .context("parse spaces/list result")
    }

    /// Register a vault directory as a pull source on the daemon and run one
    /// sync pass (native RPC — not an MCP tool). The daemon adopts the
    /// directory into a debounced watcher; registration survives restarts.
    pub async fn sync_run(&mut self, dir: &str, space: &str) -> Result<SyncOutcome> {
        let v = self
            .call_rpc_json("sync/run", json!({ "dir": dir, "space": space }))
            .await?;
        serde_json::from_value(v).context("parse sync/run result")
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
    fn default_client_version_is_compiled_in() {
        // Sanity check: the macro embedded the crate name + version.
        assert!(DEFAULT_CLIENT_VERSION.contains("oxibrain-client"));
    }
}
