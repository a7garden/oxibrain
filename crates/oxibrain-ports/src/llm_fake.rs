//! FakeLlmPort — a test-only LLM adapter that returns canned responses.
//! Used by the `fast` eval suite and integration tests. Deterministic, no network.

use crate::error::BrainError;
use crate::llm::{LlmCapabilities, LlmPort, LlmRequest, LlmResponse};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// A test-only LLM port. Canned responses are keyed by a substring of the prompt;
/// the first matching key wins. If no key matches, returns an error.
///
/// `enable_grammar()` flips the advertised capabilities to grammar-constrained
/// and counts `generate_constrained` calls, so tests can prove the pipeline
/// took the GBNF branch (§9.4) rather than the schema-and-repair path.
pub struct FakeLlmPort {
    responses: Mutex<Vec<(String, LlmResponse)>>,
    grammar: AtomicBool,
    constrained_calls: AtomicUsize,
}

impl FakeLlmPort {
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
            grammar: AtomicBool::new(false),
            constrained_calls: AtomicUsize::new(0),
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

    /// Advertise GBNF grammar support; `generate_constrained` then serves the
    /// canned responses and is counted.
    pub fn enable_grammar(&self) {
        self.grammar.store(true, Ordering::SeqCst);
    }

    /// How many `generate_constrained` calls were made.
    pub fn constrained_calls(&self) -> usize {
        self.constrained_calls.load(Ordering::SeqCst)
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

    async fn generate_constrained(
        &self,
        req: LlmRequest,
        _grammar: &str,
    ) -> Result<LlmResponse, BrainError> {
        if !self.grammar.load(Ordering::SeqCst) {
            return Err(BrainError::Provider {
                retryable: false,
                message: "FakeLlmPort: grammar not enabled (call enable_grammar)".into(),
            });
        }
        self.constrained_calls.fetch_add(1, Ordering::SeqCst);
        self.complete(req).await
    }

    fn capabilities(&self) -> LlmCapabilities {
        // `enable_grammar()` flips the GBNF flag; the additive remote
        // mechanism flags stay false — the fake deliberately advertises only
        // what it has been configured to satisfy.
        LlmCapabilities {
            grammar: self.grammar.load(Ordering::SeqCst),
            structured_output: false,
            tool_call: false,
            json_schema: false,
        }
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

    #[tokio::test]
    async fn grammar_mode_counts_constrained_calls() {
        let fake = FakeLlmPort::new();
        assert!(!fake.capabilities().grammar);
        fake.enable_grammar();
        assert!(fake.capabilities().grammar);
        fake.respond_to(
            "content",
            LlmResponse {
                text: r#"{"claims":[]}"#.into(),
                raw: serde_json::Value::Null,
            },
        );
        let req = LlmRequest {
            model: "test".into(),
            system: None,
            prompt: "some content here".into(),
            json_schema: None,
            max_tokens: 100,
        };
        let resp = fake
            .generate_constrained(req, "root ::= ...")
            .await
            .unwrap();
        assert_eq!(resp.text, r#"{"claims":[]}"#);
        assert_eq!(fake.constrained_calls(), 1);
    }

    #[tokio::test]
    async fn constrained_without_enable_is_provider_error() {
        let fake = FakeLlmPort::new();
        let req = LlmRequest {
            model: "test".into(),
            system: None,
            prompt: "x".into(),
            json_schema: None,
            max_tokens: 1,
        };
        assert!(
            fake.generate_constrained(req, "root ::= ...")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn additive_capability_flags_default_to_false() {
        let fake = FakeLlmPort::new();
        let caps = fake.capabilities();
        assert!(!caps.tool_call);
        assert!(!caps.json_schema);
        assert!(!caps.structured_output);
        assert!(!caps.grammar);
    }
}
