//! oxibrain-core: the engine. Knows nothing of MCP/HTTP/CLI (DESIGN.md P6).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod canonical;
pub mod id;
pub mod types;
pub mod interval;

pub use interval::{clip, merge_overlapping, overlaps, Interval};

pub mod knowledge;
pub mod registry;

pub use canonical::{canonical_bytes, canonical_json_value, canonicalize_value};
pub use id::{
    Id, assertion_id, content_hash, entity_id, entity_key_id, entity_merge_id, episode_id,
    mention_id, normalize_content, statement_id,
};
pub use knowledge::{
    Assertion, Belief, BeliefStatus, Entity, EntityKey, EntityMerge, KeyOrigin, Mention,
    MentionRole, MergeDecision, Object, Polarity, ResolutionMethod, Statement, Support,
    TypedValue, claim_repr, object_repr,
};
pub use types::{ContentHash, Episode, EpisodeKind, SourceRef, Space, TrustTier};

pub use registry::{
    core_v1, Cardinality, Invalidation, LiteralType, ObjectKind, PredicateDef, Temporality,
    CORE_V1_MAJOR, CORE_V1_MINOR,
};
pub mod resolution;

pub mod fold;
pub use fold::{fold, StatementEntry};

pub mod retrieval;
pub mod lifecycle;
pub mod context;

pub use retrieval::{
    Direction, DropReason, DroppedItem, PredicateFilter, Query, QueryMode,
    RankedItem, RankingResult, SearchHit, SearchTarget, Strategy, TraversalEdge,
    TraversalNode, TraversalResult, TraversalSpec,
};
pub use lifecycle::{CompactionConfig, DecayConfig, SalienceEntry, salience};
pub use context::{
    ContextBudget, ContextLayer, ContextResult, LayerKind, estimate_tokens,
};
