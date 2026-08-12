//! MCP sampling: an `LlmPort` backed by the client's model (DESIGN §12.3).
//!
//! A standalone user with Claude Desktop already has a model. Sampling lets the
//! server ask the client to run a completion (`sampling/createMessage`), so
//! extraction works without an API key. The trade-off — it routes episode
//! content through the client's provider — is why `Sample` is a separate
//! capability, off by default, per token and per space, and audited.
//!
//! This module provides:
//! - [`SessionHandle`] — a bidirectional channel for sending server-initiated
//!   JSON-RPC requests to the client and awaiting the response.
//! - [`SamplingLlmPort`] — an `LlmPort` that delegates `complete()` to the
//!   client via `sampling/createMessage`.

use oxibrain_ports::{BrainError, LlmPort, LlmRequest, LlmResponse};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long to wait for a sampling response before giving up (§12.3: client
/// disconnect / refusal is an ordinary outcome, not a hard error).
const SAMPLING_TIMEOUT: Duration = Duration::from_secs(120);

/// Handle for sending JSON-RPC requests to the client during a session and
/// awaiting the response. Cloned cheaply (channel + Arc).
///
/// Created once per session by the bidirectional session loop. The read loop
/// resolves pending requests by matching response `id`s against the pending map.
pub struct SessionHandle {
    /// Outbound channel: messages to write to the client (responses AND
    /// server-initiated requests).
    pub(crate) outbound: mpsc::UnboundedSender<Value>,
    /// Pending server-initiated requests, keyed by id. The read loop removes
    /// and resolves these when the matching response arrives.
    pub(crate) pending: Mutex<HashMap<i64, oneshot::Sender<Value>>>,
    next_id: AtomicI64,
}

impl SessionHandle {
    /// Create a new session handle bound to an outbound channel.
    pub(crate) fn new(outbound: mpsc::UnboundedSender<Value>) -> Self {
        Self {
            outbound,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
        }
    }
    /// Send a JSON-RPC request to the client and await the response.
    ///
    /// Returns the full response value (`{jsonrpc, id, result}` or
    /// `{jsonrpc, id, error}`). The caller extracts `result` or handles `error`.
    ///
    /// On timeout or disconnect, returns a **retryable** `BrainError::Provider`
    /// (§12.3: client disconnect mid-call is an ordinary retry, not a hard
    /// error).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, BrainError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.pending.lock().map_err(|e| BrainError::Provider {
                retryable: false,
                message: format!("pending map lock: {e}"),
            })?;
            guard.insert(id, tx);
        }

        let req = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        self.outbound.send(req).map_err(|_| BrainError::Provider {
            retryable: true,
            message: "client disconnected before sampling response".into(),
        })?;

        match tokio::time::timeout(SAMPLING_TIMEOUT, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                self.pending.lock().ok().and_then(|mut g| g.remove(&id));
                Err(BrainError::Provider {
                    retryable: true,
                    message: "sampling response channel dropped".into(),
                })
            }
            Err(_) => {
                self.pending.lock().ok().and_then(|mut g| g.remove(&id));
                Err(BrainError::Provider {
                    retryable: true,
                    message: "sampling timed out (client did not respond within 120s)".into(),
                })
            }
        }
    }
}

/// An `LlmPort` backed by the MCP client's model via `sampling/createMessage`.
///
/// Maps `LlmRequest` → MCP sampling params, sends the request through the
/// [`SessionHandle`], and maps the sampling response back to `LlmResponse`.
pub struct SamplingLlmPort {
    session: std::sync::Arc<SessionHandle>,
}

impl SamplingLlmPort {
    pub fn new(session: std::sync::Arc<SessionHandle>) -> Self {
        Self { session }
    }
}

#[async_trait::async_trait]
impl LlmPort for SamplingLlmPort {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        // Map LlmRequest → MCP sampling/createMessage params.
        let mut messages = Vec::new();
        // If there's a system prompt, include it in the user message preamble
        // (MCP sampling has a separate systemPrompt field, but the extraction
        // prompt is already self-contained).
        let user_text = match &req.system {
            Some(system) => format!("{system}\n\n{0}", req.prompt),
            None => req.prompt.clone(),
        };
        messages.push(json!({
            "role": "user",
            "content": { "type": "text", "text": user_text }
        }));

        let params = json!({
            "messages": messages,
            "maxTokens": req.max_tokens,
        });

        let response = self
            .session
            .request("sampling/createMessage", params)
            .await?;

        // Check for a protocol-level error from the client.
        if let Some(err) = response.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("client returned an error");
            // §12.3: sampling refusal by client policy is an ordinary outcome
            // (retryable), not a hard error.
            return Err(BrainError::Provider {
                retryable: true,
                message: format!("sampling refused: {msg}"),
            });
        }

        // Extract the text from result.content.
        let result = response.get("result").ok_or_else(|| BrainError::Provider {
            retryable: false,
            message: "sampling response missing 'result'".into(),
        })?;

        let text = result
            .get("content")
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .or_else(|| result.get("content").and_then(|c| c.as_str()))
            .ok_or_else(|| BrainError::Provider {
                retryable: false,
                message: "sampling response missing text content".into(),
            })?;

        Ok(LlmResponse {
            text: text.to_string(),
            raw: response,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock session: receives outbound requests and delivers canned responses
    /// back through the pending map (simulating the session read loop).
    struct MockSession {
        handle: std::sync::Arc<SessionHandle>,
        rx: mpsc::UnboundedReceiver<Value>,
    }

    impl MockSession {
        fn new() -> Self {
            let (tx, rx) = mpsc::unbounded_channel();
            Self {
                handle: std::sync::Arc::new(SessionHandle::new(tx)),
                rx,
            }
        }
    }

    #[tokio::test]
    async fn sampling_maps_request_and_response() {
        let mut mock = MockSession::new();
        let port = SamplingLlmPort::new(mock.handle.clone());

        // Client task: receive the sampling request and deliver the response
        // back through the pending map (simulating the session read loop).
        tokio::spawn(async move {
            let req = mock.rx.recv().await.expect("request received");
            let id = req["id"].as_i64().unwrap();
            assert_eq!(req["method"], "sampling/createMessage");
            assert_eq!(req["params"]["messages"][0]["role"], "user");
            let response = json!({"jsonrpc":"2.0","id":id,"result":{
                "role":"assistant",
                "content":{"type":"text","text":"{\"claims\":[]}"},
                "model":"test-model"
            }});
            if let Ok(mut guard) = mock.handle.pending.lock() {
                if let Some(sender) = guard.remove(&id) {
                    let _ = sender.send(response);
                }
            }
        });

        let req = LlmRequest {
            model: "test".into(),
            system: Some("You are an extractor.".into()),
            prompt: "Alice works at Acme.".into(),
            json_schema: None,
            max_tokens: 100,
        };
        let resp = port.complete(req).await.unwrap();
        assert_eq!(resp.text, "{\"claims\":[]}");
        assert_eq!(resp.raw["result"]["model"], "test-model");
    }

    #[tokio::test]
    async fn sampling_client_error_is_retryable() {
        let mut mock = MockSession::new();
        let port = SamplingLlmPort::new(mock.handle.clone());

        tokio::spawn(async move {
            let req = mock.rx.recv().await.unwrap();
            let id = req["id"].as_i64().unwrap();
            let response = json!({"jsonrpc":"2.0","id":id,
                                   "error":{"code":-32603,"message":"policy denied"}});
            if let Ok(mut guard) = mock.handle.pending.lock() {
                if let Some(sender) = guard.remove(&id) {
                    let _ = sender.send(response);
                }
            }
        });

        let req = LlmRequest {
            model: "test".into(),
            system: None,
            prompt: "test".into(),
            json_schema: None,
            max_tokens: 10,
        };
        let err = port.complete(req).await.unwrap_err();
        assert!(err.retryable(), "client refusal should be retryable");
        assert!(err.to_string().contains("sampling refused"));
    }
}
