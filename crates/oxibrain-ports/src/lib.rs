//! Ports: traits owned by oxibrain, implementations pluggable.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod clock;
pub mod embedding;
pub mod error;
pub mod llm;
pub mod llm_fake;
pub mod rerank;
pub mod time;
pub mod tokenizer;
pub use clock::{ClockPort, FakeClock, SystemClock};
pub use embedding::EmbeddingPort;
pub use error::BrainError;
pub use llm::{LlmCapabilities, LlmPort, LlmRequest, LlmResponse};
pub use llm_fake::FakeLlmPort;
pub use rerank::{RerankItem, RerankPort};
pub use time::{TIME_MAX, TIME_MIN, Timestamp};
pub use tokenizer::{CharTokenizer, TokenizerPort};
