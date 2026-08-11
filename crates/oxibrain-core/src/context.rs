//! Context assembly types (DESIGN §9.5).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextLayer {
    pub kind: LayerKind,
    pub text: String,
    pub estimated_tokens: usize,
    pub provenance: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    PinnedFacts,
    HighSalienceBeliefs,
    QueryNeighborhood,
    RecentEpisodes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResult {
    pub layers: Vec<ContextLayer>,
    pub total_tokens: usize,
    pub budget: ContextBudget,
    pub truncated: bool,
}

pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}