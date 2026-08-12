//! Shared LLM provider construction from environment variables.
//!
//! Used by `extract` and `reextract`. A standalone user sets one of:
//!   - `OXIBRAIN_LLM_PROVIDER=anthropic` (+ `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=openai`     (+ `OPENAI_API_KEY`, `OPENAI_MODEL`)
//!
//! `OXIBRAIN_MODEL` is a fallback for either. The mechanism (tool-call vs
//! json-schema) follows the provider — Anthropic uses forced tool calls,
//! OpenAI uses native json_schema structured output (DESIGN §7.4).

use oxibrain_core::extraction::ExtractMechanism;
use oxibrain_ports::LlmPort;
use std::sync::Arc;

/// Build an LLM port from the environment.
///
/// Returns `(port, model_id, mechanism)` — the model_id feeds the
/// `ExtractorConfig` so the cache key reflects the model actually used.
pub fn from_env() -> anyhow::Result<(Arc<dyn LlmPort>, String, ExtractMechanism)> {
    let provider =
        std::env::var("OXIBRAIN_LLM_PROVIDER").unwrap_or_else(|_| "anthropic".to_string());
    match provider.as_str() {
        "anthropic" => {
            let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
                anyhow::anyhow!("ANTHROPIC_API_KEY not set (required for extraction)")
            })?;
            let model = std::env::var("ANTHROPIC_MODEL")
                .or_else(|_| std::env::var("OXIBRAIN_MODEL"))
                .unwrap_or_else(|_| "claude-sonnet-4-5".to_string());
            Ok((
                Arc::new(oxibrain_llm_http::AnthropicLlm::new(key, model.clone())),
                model,
                ExtractMechanism::ToolCall,
            ))
        }
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY not set (required for extraction)"))?;
            let model = std::env::var("OPENAI_MODEL")
                .or_else(|_| std::env::var("OXIBRAIN_MODEL"))
                .unwrap_or_else(|_| "gpt-4o".to_string());
            Ok((
                Arc::new(oxibrain_llm_http::OpenAiLlm::new(key, model.clone())),
                model,
                ExtractMechanism::JsonSchema,
            ))
        }
        other => {
            anyhow::bail!("unknown OXIBRAIN_LLM_PROVIDER={other} (expected: anthropic|openai)")
        }
    }
}

/// Build a default extractor config from the env-resolved model + mechanism.
pub fn config(
    model_id: String,
    mechanism: ExtractMechanism,
) -> oxibrain_core::extraction::ExtractorConfig {
    use oxibrain_core::registry::CORE_V1_MAJOR;
    oxibrain_core::extraction::ExtractorConfig {
        model_id,
        prompt_version: 1,
        registry_major: CORE_V1_MAJOR,
        mechanism,
        max_tokens: 8192,
    }
}
