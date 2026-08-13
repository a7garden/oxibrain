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
//! Platform: Unix-only (Unix-domain sockets). On non-Unix targets the crate
//! compiles but all methods return `UnsupportedPlatform`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

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
        self.call_tool_json("stats", json!({ "space": space })).await
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
}
