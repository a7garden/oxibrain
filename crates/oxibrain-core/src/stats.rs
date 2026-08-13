//! Space statistics — counts for dashboards and the `stats` MCP tool.

use serde::{Deserialize, Serialize};

/// Aggregate counts for a space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceStats {
    /// Number of episodes recorded in the space.
    pub episodes: i64,
    /// Number of entities (excluding merged-away) in the space.
    pub entities: i64,
    /// Number of statements in the space.
    pub statements: i64,
    /// Number of contradicted statements in the space.
    pub contradictions: usize,
}
