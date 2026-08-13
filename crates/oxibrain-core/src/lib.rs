//! oxibrain-core: the engine. Knows nothing of MCP/HTTP/CLI (ARCHITECTURE.md P6).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod canonical;
pub mod id;
pub mod interval;
pub mod types;

pub use interval::{Interval, clip, merge_overlapping, overlaps};

pub mod knowledge;
pub mod registry;

pub use canonical::{canonical_bytes, canonical_json_value, canonicalize_value};
pub use id::{
    Id, assertion_id, chunk_id, content_hash, entity_id, entity_key_id, entity_merge_id,
    episode_id, mention_id, normalize_content, statement_id,
};
pub use knowledge::{
    Assertion, Belief, BeliefStatus, Entity, EntityKey, EntityMerge, KeyOrigin, Mention,
    MentionRole, MergeDecision, Object, Polarity, ResolutionMethod, Statement, Support, TypedValue,
    claim_repr, object_repr,
};
pub use types::{ContentHash, Episode, EpisodeKind, SourceRef, Space, TrustTier};

pub use registry::{
    CORE_V1_MAJOR, CORE_V1_MINOR, Cardinality, Invalidation, LiteralType, ObjectKind, PredicateDef,
    Temporality, core_v1,
};
pub mod resolution;

pub mod fold;
pub use fold::{StatementEntry, fold};

pub mod confidence;
pub mod context;
pub mod eval;
pub mod extraction;
pub mod lifecycle;
pub mod rank;
pub mod retrieval;
pub mod security;
pub mod uncertainty;
pub use uncertainty::{Uncertainty, UncertaintyInput, compute as compute_uncertainty};
pub mod pipeline;
pub use pipeline::{Outcome as PipelineOutcome, Stage as PipelineStage, step as pipeline_step};
pub use rank::{
    Channel, ChannelRank, ChannelResult, DropReason, DroppedItem, Filters, Fusion, LexIndex,
    RankedItem, RankingResult, Rerank, Retrieval, RetrievalInput, SeedPolicy, TargetFacts,
    TargetId, TargetSet, TrustPolicy, VecSpace, explain_item, rank,
};
pub mod pack;
pub use pack::{
    BeliefForm, ContextInput, EpisodeExcerpt, PackPolicy, ProfileFact, RenderedBelief,
    RenderedEdge, Reserve, SummaryWithUncertainty, pack,
};
pub mod chunking;
pub use chunking::{Chunk, ChunkPolicy, render_context_prefix, short_ts, split_into_chunks};
pub mod stats;
pub use security::{
    AuditEntry, Capability, CapabilitySet, RedactTarget, RedactionClosure, RedactionResult, Scope,
    TokenInfo,
};

pub use context::{ContextBudget, ContextLayer, ContextResult, LayerKind, estimate_tokens_rough};
pub use lifecycle::{CompactionConfig, DecayConfig, SalienceEntry, salience};
pub use retrieval::{
    Direction, PredicateFilter, Query, QueryMode, SearchHit, SearchTarget, Strategy, TraversalEdge,
    TraversalNode, TraversalResult, TraversalSpec,
};
pub use stats::SpaceStats;
