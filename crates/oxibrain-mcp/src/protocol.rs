//! Minimal MCP-over-JSON-RPC-2.0 framing (DESIGN §18 fallback).
//!
//! The stdio transport is newline-delimited JSON-RPC 2.0: one request or
//! notification per line. This module owns message parsing and the MCP-specific
//! response shapes. There is no external protocol dependency.

use serde_json::{Value, json};

// ── JSON-RPC 2.0 error codes ──────────────────────────────────────────────

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// Implementation-defined: token lacks the required capability/scope.
pub const UNAUTHORIZED: i64 = -32001;
/// Implementation-defined: the Oxi Foundation `handshake` was rejected
/// (incompatible protocol, store format too old, etc.).
pub const INCOMPATIBLE_PROTOCOL: i64 = -32002;
pub const INTERNAL_ERROR: i64 = -32603;

/// A parsed JSON-RPC request or notification. `id == None` marks a notification
/// (no response expected).
#[derive(Debug, Clone)]
pub struct Message {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

impl Message {
    /// Parse one line of JSON-RPC.
    ///
    /// On failure returns `(synthetic_id, code, message)` so the caller can emit
    /// an error response. The id is taken from the (possibly-malformed) payload
    /// when available, else `None`.
    pub fn parse(line: &str) -> Result<Self, (Option<Value>, i64, String)> {
        let v: Value = serde_json::from_str(line)
            .map_err(|e| (None, PARSE_ERROR, format!("parse error: {e}")))?;
        let id = v.get("id").cloned();
        let method = v
            .get("method")
            .and_then(|m| m.as_str())
            .ok_or((id.clone(), INVALID_REQUEST, "missing 'method'".into()))?
            .to_string();
        let params = v.get("params").cloned();
        Ok(Self { id, method, params })
    }

    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

// ── Response builders ─────────────────────────────────────────────────────

/// Build a JSON-RPC success response value: `{jsonrpc, id, result}`.
pub fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC error response value: `{jsonrpc, id, error:{code,message}}`.
pub fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// Build a JSON-RPC error response with a structured `data` payload.
///
/// Used by the `handshake` method to surface a typed rejection
/// (`HandshakeError` from `oxibrain_client::protocol`) so the client can
/// recover without parsing free-form strings.
pub fn error_with_data(id: Value, code: i64, message: impl Into<String>, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into(), "data": data }
    })
}

/// MCP `tools/call` success result: a single text content block.
pub fn text_result(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }] })
}

/// MCP `tools/call` error result — the tool ran but failed. Still a successful
/// JSON-RPC response; protocol-level errors use `error()` instead.
pub fn tool_error(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_with_params() {
        let m = Message::parse(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"search"}}"#,
        )
        .unwrap();
        assert_eq!(m.id, Some(Value::from(7)));
        assert_eq!(m.method, "tools/call");
        assert_eq!(m.params.as_ref().unwrap()["name"], "search");
        assert!(!m.is_notification());
    }

    #[test]
    fn parse_notification_has_no_id() {
        let m =
            Message::parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert!(m.id.is_none());
        assert!(m.is_notification());
    }

    #[test]
    fn parse_garbage_is_parse_error() {
        let err = Message::parse("not json").unwrap_err();
        assert_eq!(err.1, PARSE_ERROR);
        assert!(err.0.is_none());
    }

    #[test]
    fn parse_missing_method_is_invalid_request() {
        let err = Message::parse(r#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        assert_eq!(err.1, INVALID_REQUEST);
        assert_eq!(err.0, Some(Value::from(1)));
    }
}
