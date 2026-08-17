//! LLM inference port. Adapters ship per-provider (M3 HTTP, M7 local).

use crate::error::BrainError;
use serde::{Deserialize, Serialize};

/// Advertises what an [`LlmPort`] implementation can do. The extraction
/// pipeline checks `grammar` to decide between constrained decoding and
/// schema-and-repair (§9.5). Oxi Foundation v1 profiles use the additive
/// `tool_call` / `json_schema` flags to declare which extraction mechanism
/// a remote model can satisfy; the CLI boundary compares those against the
/// configured extraction mechanism before allowing the profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmCapabilities {
    /// Supports GBNF grammar-constrained decoding (§9.4, D28).
    pub grammar: bool,
    /// Supports JSON Schema structured output natively.
    pub structured_output: bool,
    /// Supports forced tool calls (Anthropic-style `tool_use`).
    ///
    /// Additive (Oxi Foundation v1): profiles declare which output mechanism
    /// a remote model can satisfy; the host compares that against the
    /// configured extraction mechanism before allowing the profile. Defaults
    /// to `false` so existing adapters retain their behaviour until they
    /// opt-in truthfully.
    #[serde(default)]
    pub tool_call: bool,
    /// Supports provider-native JSON Schema structured output (OpenAI
    /// `response_format` / json_schema strict). Additive; defaults to `false`.
    #[serde(default)]
    pub json_schema: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    pub json_schema: Option<serde_json::Value>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub text: String,
    pub raw: serde_json::Value,
}

#[async_trait::async_trait]
pub trait LlmPort: Send + Sync {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError>;

    /// Constrained generation with a GBNF grammar (§9.4, D28). Adapters that
    /// cannot honour a grammar return a non-retryable `Provider` error; the
    /// caller falls back to schema-and-repair and records the mechanism in
    /// ExtractorId (§9.5).
    async fn generate_constrained(
        &self,
        req: LlmRequest,
        grammar: &str,
    ) -> Result<LlmResponse, BrainError> {
        let _ = (req, grammar);
        Err(BrainError::Provider {
            retryable: false,
            message: "grammar-constrained generation not supported by this adapter".into(),
        })
    }

    /// Returns the adapter's capabilities. Default: nothing supported.
    fn capabilities(&self) -> LlmCapabilities {
        LlmCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additive_fields_default_to_false() {
        let caps = LlmCapabilities::default();
        assert!(!caps.tool_call);
        assert!(!caps.json_schema);
        // Existing fields unchanged.
        assert!(!caps.grammar);
        assert!(!caps.structured_output);
    }

    #[test]
    fn additive_fields_round_trip_through_serde() {
        let caps = LlmCapabilities {
            grammar: true,
            structured_output: false,
            tool_call: true,
            json_schema: false,
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: LlmCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }

    #[test]
    fn additive_fields_default_when_omitted_in_json() {
        let json = r#"{"grammar":true,"structured_output":true}"#;
        let caps: LlmCapabilities = serde_json::from_str(json).unwrap();
        assert!(caps.grammar);
        assert!(caps.structured_output);
        assert!(!caps.tool_call);
        assert!(!caps.json_schema);
    }
}
