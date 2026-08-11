//! HTTP LLM adapters: Anthropic (tool-use) and OpenAI (json_schema).
//! Both implement `oxibrain_ports::LlmPort`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod anthropic;
pub mod openai;

pub use anthropic::AnthropicLlm;
pub use openai::OpenAiLlm;
