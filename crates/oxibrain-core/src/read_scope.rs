//! Canonical read validation shared by every transport.
use crate::retrieval::SearchPlane;
use oxibrain_ports::{BrainError, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadScope {
    pub space: String,
    pub as_of: Option<Timestamp>,
    pub known_at: Option<Timestamp>,
    pub min_confidence: f32,
}
impl ReadScope {
    pub fn current(space: impl Into<String>) -> Self {
        Self {
            space: space.into(),
            as_of: None,
            known_at: None,
            min_confidence: 0.0,
        }
    }
    pub fn validate(
        &self,
        planes: &BTreeSet<SearchPlane>,
        now: Timestamp,
    ) -> Result<(), BrainError> {
        if planes.is_empty()
            || self.space.is_empty()
            || !self.min_confidence.is_finite()
            || !(0.0..=1.0).contains(&self.min_confidence)
        {
            return Err(BrainError::Invalid("invalid read scope".into()));
        }
        if self.known_at.is_some_and(|k| k > now) {
            return Err(BrainError::Invalid("known_at exceeds request time".into()));
        }
        if planes.contains(&SearchPlane::Documents)
            && (self.as_of.is_some() || self.known_at.is_some())
        {
            return Err(BrainError::UnsupportedTemporalScope {
                plane: "documents".into(),
                fields: [("as_of", self.as_of), ("known_at", self.known_at)]
                    .into_iter()
                    .filter_map(|(name, v)| v.map(|_| name.into()))
                    .collect(),
            });
        }
        if !planes.contains(&SearchPlane::Memory) && self.min_confidence > 0.0 {
            return Err(BrainError::Invalid(
                "min_confidence requires the memory plane".into(),
            ));
        }
        Ok(())
    }
}

/// Versioned engineering bounds, not learned semantic thresholds. Storage window
/// policy is separate and versioned with the raw-index checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReadPolicy {
    pub channel_depth: usize,
    pub candidate_cap: usize,
    pub query_terms: usize,
    pub truth_bytes: usize,
    pub materialization_bytes: usize,
    pub reconcile_entries: usize,
    pub metadata_bytes: usize,
    pub explanation_bytes: usize,
    pub profile_tokens: usize,
    pub recency_tokens: usize,
    pub semantic_sources: bool,
    /// Compact response-scoped citation IDs; full source maps stay in evidence.
    pub compact_evidence: bool,
    /// Maximum raw-source prefix bytes to accompany a later ranked chunk; zero disables.
    pub source_context_bytes: usize,
    pub dense_scan_vectors: usize,
    pub truth_group_cap: usize,
}
impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            channel_depth: 64,
            candidate_cap: 256,
            query_terms: 32,
            truth_bytes: 8 * 1024 * 1024,
            materialization_bytes: 8 * 1024 * 1024,
            reconcile_entries: 4096,
            metadata_bytes: 65536,
            explanation_bytes: 16384,
            profile_tokens: 300,
            recency_tokens: 200,
            semantic_sources: true,
            compact_evidence: true,
            source_context_bytes: 768,
            dense_scan_vectors: 16384,
            truth_group_cap: 256,
        }
    }
}
impl ReadPolicy {
    pub const VERSION: &'static str = "read-policy/5";
    pub fn validate(&self) -> Result<(), BrainError> {
        if [
            self.channel_depth,
            self.candidate_cap,
            self.query_terms,
            self.truth_bytes,
            self.materialization_bytes,
            self.reconcile_entries,
            self.metadata_bytes,
            self.dense_scan_vectors,
            self.truth_group_cap,
        ]
        .contains(&0)
        {
            return Err(BrainError::Config(
                "read policy bounds must be nonzero".into(),
            ));
        }
        Ok(())
    }
}
