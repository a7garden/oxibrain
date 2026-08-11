//! Ports: traits owned by oxibrain, implementations pluggable.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod clock;
pub mod embedding;
pub mod error;
pub mod llm;
pub mod time;

pub use clock::{ClockPort, FakeClock, SystemClock};
pub use error::BrainError;
pub use llm::{LlmPort, LlmRequest, LlmResponse};
pub use time::{TIME_MAX, TIME_MIN, Timestamp};
