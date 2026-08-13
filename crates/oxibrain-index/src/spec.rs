//! Shared specification types used by both the core query layer and the index
//! algorithm layer. Lives in `oxibrain-index` (the lower crate per §18 rule 1)
//! so that `oxibrain-core` can depend on `oxibrain-index` without a cycle.

use serde::{Deserialize, Serialize};

/// Edge direction for graph traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Out,
    In,
    Both,
}

/// Predicate filter for graph traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateFilter {
    AllowAll,
    Allow(Vec<String>),
    Deny(Vec<String>),
}

impl PredicateFilter {
    pub fn allows(&self, predicate: &str) -> bool {
        match self {
            PredicateFilter::AllowAll => true,
            PredicateFilter::Allow(list) => list.iter().any(|p| p == predicate),
            PredicateFilter::Deny(list) => !list.iter().any(|p| p == predicate),
        }
    }
}
