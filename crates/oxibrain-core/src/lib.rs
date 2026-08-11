//! oxibrain-core: the engine. Knows nothing of MCP/HTTP/CLI (DESIGN.md P6).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod canonical;
pub mod id;
pub mod types;

pub use canonical::{canonical_bytes, canonical_json_value, canonicalize_value};
pub use id::{Id, content_hash, episode_id, normalize_content};
pub use types::{ContentHash, Episode, EpisodeKind, SourceRef, Space, TrustTier};
