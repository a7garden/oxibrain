//! oxibrain-core: the engine. Knows nothing of MCP/HTTP/CLI (DESIGN.md P6).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod canonical;
pub mod id;
pub mod types;
pub mod interval;

pub use interval::{clip, merge_overlapping, overlaps, Interval};

pub mod knowledge;

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

