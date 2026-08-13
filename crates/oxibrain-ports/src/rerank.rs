//! Reranking port (DESIGN §11.4, M10 10.5). A cross-encoder scores
//! (query, item) pairs to produce a more accurate relevance ordering than
//! pure lexical/vector fusion can achieve.
//!
//! Unlike the pure rerankers in `oxibrain-core::rank` (Corroboration, MMR),
//! the cross-encoder is async and I/O-bound — it is applied by the store
//! layer after `rank()`, not inside the pure `apply_rerank`.

use crate::error::BrainError;
use serde::{Deserialize, Serialize};

/// One item to be scored by the cross-encoder. `text` is the document
/// representation; `score` is the pre-rerank score (from fusion + pure
/// rerankers). The cross-encoder overwrites `score` with its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankItem {
    pub id: String,
    pub text: String,
    pub score: f64,
}

/// Cross-encoder rerank port. Implementations score (query, item) pairs
/// and return items sorted by relevance descending.
#[async_trait::async_trait]
pub trait RerankPort: Send + Sync {
    /// Score each item against the query and return them sorted by the
    /// cross-encoder's score descending.
    async fn rerank(
        &self,
        query: &str,
        items: Vec<RerankItem>,
    ) -> Result<Vec<RerankItem>, BrainError>;
}
