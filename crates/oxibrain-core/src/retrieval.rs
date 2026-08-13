//! Retrieval types: Query, TraversalSpec, and legacy store-side handles.
//!
//! The M8 `Retrieval` type and its `rank()` live in [`crate::rank`]. This
//! module keeps the legacy `Query` / `QueryMode` / `SearchHit` / `SearchTarget`
//! types that the pre-M8 `hybrid_query` path in `oxibrain-store::query`
//! consumes. The presets in `crate::rank::Retrieval::hybrid()` etc. translate
//! the same `QueryMode` strings into a `Retrieval` so the MCP `mode`
//! parameter keeps working without a server-side rename (F29).

use crate::knowledge::{EntityId, StatementId};
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

// Re-export the M8 rank types so existing import paths
// (`oxibrain_core::retrieval::RankingResult` etc.) continue to resolve.
pub use crate::rank::{DropReason, DroppedItem, RankedItem, RankingResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub mode: QueryMode,
    pub space: String,
    #[serde(default)]
    pub as_of: Option<Timestamp>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub min_confidence: f32,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    Hybrid,
    Lexical,
    LexicalVector,
    /// Dense embedding KNN via sqlite-vec. Requires a configured embedder;
    /// without one, querying in this mode returns an explicit error (§7.6).
    Dense,
    Graph,
    Community,
}

impl QueryMode {
    /// Translate the M7 string enum to an M8 preset name. The preset lives in
    /// `crate::rank::Retrieval::hybrid/lexical/semantic/graph/community`.
    pub fn to_preset(self) -> &'static str {
        match self {
            QueryMode::Hybrid => "hybrid",
            QueryMode::Lexical => "lexical",
            QueryMode::LexicalVector => "lexical", // TF-IDF KNN collapses into lexical
            QueryMode::Dense => "semantic",
            QueryMode::Graph => "graph",
            QueryMode::Community => "community",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub target: SearchTarget,
    pub score: f64,
    pub mode: QueryMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchTarget {
    Episode { id: String },
    Statement { id: StatementId },
    Entity { id: EntityId },
}

// --- Traversal ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalSpec {
    pub start: Vec<EntityId>,
    pub max_depth: u8,
    pub max_nodes: u32,
    pub predicates: PredicateFilter,
    pub direction: Direction,
    #[serde(default)]
    pub valid_at: Option<Timestamp>,
    pub min_confidence: f32,
    pub strategy: Strategy,
}

impl Default for TraversalSpec {
    fn default() -> Self {
        Self {
            start: Vec::new(),
            max_depth: 3,
            max_nodes: 256,
            predicates: PredicateFilter::AllowAll,
            direction: Direction::Both,
            valid_at: None,
            min_confidence: 0.0,
            strategy: Strategy::Bfs,
        }
    }
}
// Direction and PredicateFilter live in oxibrain-index (spec.rs) per §18
// rule 1 (core depends on index, not the reverse). Re-exported here so
// existing `oxibrain_core::retrieval::Direction` paths continue to work.
pub use oxibrain_index::{Direction, PredicateFilter};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Strategy {
    Bfs,
    ShortestPath { to: EntityId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalResult {
    pub nodes: Vec<TraversalNode>,
    pub edges: Vec<TraversalEdge>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalNode {
    pub entity: EntityId,
    pub depth: u8,
    pub salience: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalEdge {
    pub from: EntityId,
    pub to: EntityId,
    pub predicate: String,
    pub statement_id: StatementId,
    pub depth: u8,
}
