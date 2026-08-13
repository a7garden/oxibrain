//! LLM inference port. Adapters ship per-provider (M3 HTTP, M7 local).

use crate::error::BrainError;
use serde::{Deserialize, Serialize};

/// Advertises what an [`LlmPort`] implementation can do. The extraction
/// pipeline checks `grammar` to decide between constrained decoding and
/// schema-and-repair (§9.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmCapabilities {
    /// Supports GBNF grammar-constrained decoding (§9.4, D28).
    pub grammar: bool,
    /// Supports JSON Schema structured output natively.
    pub structured_output: bool,
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
