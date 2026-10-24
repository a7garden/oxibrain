//! Observable candidate-channel state, separate from answerability.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelReceipt {
    pub plane: String,
    pub channel: String,
    pub eligible: bool,
    pub executed: bool,
    pub model_identity: Option<String>,
    pub indexed: Option<usize>,
    pub total: Option<usize>,
    pub considered: usize,
    pub capped: bool,
    pub limit_reached: bool,
    pub fallback: Option<String>,
    /// Number of matching query/source pairs before per-query result limits.
    /// This is not a distinct source count and is therefore separate from `total`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_occurrences: Option<usize>,
    /// Candidate-query planning details for versioned lexical expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithm_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_limit: Option<usize>,
    /// Core lexical candidates considered and retained before store queries.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub plan_candidates_examined: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub plan_candidates_selected: usize,
    /// Number of actual store query variants executed, including the exact query.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub queries_run: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}
impl ChannelReceipt {
    pub fn lexical(plane: &str, channel: &str, considered: usize, depth: usize) -> Self {
        Self {
            plane: plane.into(),
            channel: channel.into(),
            eligible: true,
            executed: true,
            considered,
            limit_reached: considered >= depth,
            ..Self::default()
        }
    }
    pub fn partial(&self) -> bool {
        self.eligible
            && (self.fallback.is_some()
                || self.capped
                || self
                    .indexed
                    .zip(self.total)
                    .is_some_and(|(n, total)| n < total))
    }
}
