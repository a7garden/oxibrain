//! Embedding port. M0 defines the trait only; adapters ship in M2/M3.

use crate::error::BrainError;

pub trait EmbeddingPort: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, BrainError>;
}
