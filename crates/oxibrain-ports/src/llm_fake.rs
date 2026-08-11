//! FakeLlmPort — a test-only LLM adapter that returns canned responses.
//! Used by the `fast` eval suite and integration tests. Deterministic, no network.

use crate::error::BrainError;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use std::sync::Mutex;

/// A test-only LLM port. Canned responses are keyed by a substring of the prompt;
/// the first matching key wins. If no key matches, returns an error.
pub struct FakeLlmPort {
    responses: Mutex<Vec<(String, LlmResponse)>>,
}

impl FakeLlmPort {
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
        }
    }

    /// Register a canned response. `key` is matched as a substring of the prompt.
    /// Registrations are checked in insertion order; first match wins.
    pub fn respond_to(&self, key: impl Into<String>, response: LlmResponse) {
        self.responses
            .lock()
            .expect("fake llm mutex")
            .push((key.into(), response));
    }
}

impl Default for FakeLlmPort {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LlmPort for FakeLlmPort {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        let entries = self.responses.lock().expect("fake llm mutex");
        for (key, resp) in entries.iter() {
            if req.prompt.contains(key.as_str()) {
                return Ok(resp.clone());
            }
        }
        Err(BrainError::Config(format!(
            "FakeLlmPort: no canned response matching the prompt ({} keys registered)",
            entries.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_matching_response() {
        let fake = FakeLlmPort::new();
        fake.respond_to(
            "Alice works on",
            LlmResponse {
                text: r#"{"claims":[]}"#.into(),
                raw: serde_json::Value::Null,
            },
        );
        let req = LlmRequest {
            model: "test".into(),
            system: None,
            prompt: "Alice works on ProjectX".into(),
            json_schema: None,
            max_tokens: 100,
        };
        let resp = fake.complete(req).await.unwrap();
        assert_eq!(resp.text, r#"{"claims":[]}"#);
    }

    #[tokio::test]
    async fn errors_when_no_match() {
        let fake = FakeLlmPort::new();
        fake.respond_to(
            "nonexistent",
            LlmResponse {
                text: "{}".into(),
                raw: serde_json::Value::Null,
            },
        );
        let req = LlmRequest {
            model: "test".into(),
            system: None,
            prompt: "totally different content".into(),
            json_schema: None,
            max_tokens: 100,
        };
        assert!(fake.complete(req).await.is_err());
    }
}
