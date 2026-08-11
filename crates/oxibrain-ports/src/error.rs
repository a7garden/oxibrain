//! Typed errors at every public boundary. anyhow is internal only.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrainError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("schema version mismatch: found {found}, expected {expected}")]
    Migration { found: i64, expected: i64 },
    #[error("store locked by another writer: {holder}")]
    Locked { holder: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("corruption: {0}")]
    Corruption(String),
    #[error("provider error (retryable: {retryable}): {message}")]
    Provider { retryable: bool, message: String },
    #[error("extraction error: {0}")]
    Extraction(String),
    #[error("budget exceeded: {0}")]
    Budget(String),
    #[error("insufficient scope: requires {required}")]
    Scope { required: String },
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

impl BrainError {
    /// True if repeating the operation might succeed (transient I/O, lock contention).
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Storage(_)
                | Self::Locked { .. }
                | Self::Provider {
                    retryable: true,
                    ..
                }
        )
    }
}
