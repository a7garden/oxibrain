//! LLM inference port. M0 defines the trait only; adapters ship in M3.

use crate::error::BrainError;
use serde::{Deserialize, Serialize};

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
}
