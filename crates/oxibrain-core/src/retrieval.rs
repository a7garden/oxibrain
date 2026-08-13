//! Retrieval types: Query, TraversalSpec, RankingResult (DESIGN §9).
//! Type definitions only — execution lives in store.

use crate::knowledge::{EntityId, StatementId};
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

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
    Graph,
    Community,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedItem {
    pub target: SearchTarget,
    pub fused_score: f64,
    pub rank: usize,
    pub mode_ranks: Vec<(QueryMode, usize)>,
    pub salience: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingResult {
    pub items: Vec<RankedItem>,
    pub dropped: Vec<DroppedItem>,
    pub total_found: usize,
    pub query: Query,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedItem {
    pub target: SearchTarget,
    pub reason: DropReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    BelowConfidenceFloor { actual: f32, floor: f32 },
    OutsideValidWindow { valid_at: Timestamp },
    TrustExcluded { tier: String },
    TruncatedByBudget { position: usize },
    BelowSalienceFloor { salience: f64, floor: f64 },
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
// Direction and PredicateFilter now live in oxibrain-index (spec.rs) per §18
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
