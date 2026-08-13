//! Identity and resolution (ARCHITECTURE.md §8, §10).
//!
//! Lexical matching uses **n-gram Jaccard** (§7.7) — order-insensitive,
//! prefix-neutral, identical in every script. The Jaro-Winkler prefix bonus
//! that systematically boosted shared surnames (F28, D30) is gone.
//!
//! Embeddings and graph context are wired but may remain zero until M9 — they
//! are **not** deleted and **not** hardcoded in the caller (that is how F13
//! happened). The caller passes the actual values; the architecture decides
//! whether they are zero.

use crate::knowledge::{EntityId, EntityKey, EntityTypeRef, ResolutionMethod};
use oxibrain_index::ngram;
use std::collections::BTreeMap;

// ─── PerType ───────────────────────────────────────────────────────────────

/// A per-entity-type value with a default for unmapped types (§10.3).
///
/// Embedding weights differ by type: low for Person/Organization (where names
/// are literal), higher for Concept (where paraphrase is normal). The default
/// covers types not explicitly configured.
#[derive(Debug, Clone)]
pub struct PerType<T: Clone> {
    default: T,
    overrides: BTreeMap<String, T>,
}

impl<T: Clone> PerType<T> {
    pub fn new(default: T) -> Self {
        Self {
            default,
            overrides: BTreeMap::new(),
        }
    }

    /// Set the value for a specific entity type.
    pub fn set(&mut self, ty: &str, value: T) {
        self.overrides.insert(ty.to_string(), value);
    }

    /// Look up the value for a type, falling back to the default.
    pub fn get(&self, ty: &str) -> &T {
        self.overrides.get(ty).unwrap_or(&self.default)
    }
}

impl PerType<f64> {
    /// Convenience: return the weight as a plain f64.
    pub fn weight(&self, ty: &str) -> f64 {
        *self.get(ty)
    }
}

// ─── Config ────────────────────────────────────────────────────────────────

/// Configuration for resolution thresholds and scoring weights (§10.1).
#[derive(Debug, Clone)]
pub struct ResolutionConfig {
    pub tau_high: f64,
    pub tau_low: f64,
    pub w_exact: f64,
    pub w_ngram: f64,
    pub w_graph: f64,
    pub w_embedding: PerType<f64>,
}

impl Default for ResolutionConfig {
    fn default() -> Self {
        Self {
            // Thresholds lowered from v1.0 (0.85 / 0.55): n-gram Jaccard gives
            // lower absolute scores than Jaro-Winkler for the same string pair.
            // Exact matches still score ≥ 1.0 via w_exact; the thresholds govern
            // the fuzzy + context zone. Calibrated against Jaccard on 3-gram
            // shingles — "alice"/"alicia" ≈ 0.36, "alice"/"alise" ≈ 0.40.
            tau_high: 0.75,
            tau_low: 0.25,
            w_exact: 1.0,
            w_ngram: 1.0,
            w_graph: 0.4,
            // Embeddings are a secondary signal for names (§10.3): low for
            // Person/Organization (names are literal), higher for Concept
            // (paraphrase is normal). The signal itself comes from the caller
            // and is zero until embeddings are available at resolution time.
            w_embedding: {
                let mut w = PerType::new(0.3);
                w.set("Person", 0.1);
                w.set("Organization", 0.1);
                w.set("Concept", 0.6);
                w
            },
        }
    }
}

// ─── Decision ──────────────────────────────────────────────────────────────

/// The resolution decision for a mention against a set of candidates.
#[derive(Debug, Clone)]
pub enum Decision {
    /// Link to existing entity. Score ≥ tau_high.
    Link {
        entity: EntityId,
        method: ResolutionMethod,
        score: f64,
    },
    /// Create a new entity. Score ≤ tau_low.
    New {
        method: ResolutionMethod,
        score: f64,
    },
    /// Create a new entity AND record a merge candidate.
    /// tau_low < score < tau_high.
    Candidate {
        new_entity: EntityId,
        existing: EntityId,
        score: f64,
    },
}

// ─── Normalize ─────────────────────────────────────────────────────────────

/// Normalize a surface form: NFKC, casefold, collapse whitespace.
/// Honorifics/suffixes per entity type are registry data (§7.6) and stripped
/// when configured; normalization is always script-neutral (P11).
pub fn normalize(surface: &str, _ty: &EntityTypeRef) -> String {
    use unicode_normalization::UnicodeNormalization;
    surface
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── Score ─────────────────────────────────────────────────────────────────

/// Compute the resolution score for a candidate.
///
/// `score = type_gate × (w_exact·is_exact + w_ngram·jaccard₃ + w_graph·ctx + w_emb·emb)`
///
/// - `type_gate` = 0.0 if types disagree (hard reject), 1.0 if they match.
/// - `jaccard₃` is n-gram Jaccard over 3-gram shingles of the normalized
///   surfaces (§7.7). Order-insensitive and prefix-neutral in every script.
/// - `ctx` is the graph-context overlap (§10.2) — evidence about the world,
///   not a spelling heuristic.
/// - `emb` is the embedding similarity (§10.3) — secondary for names, weighted
///   per type.
///
/// Result is clamped to `[0, 1]`.
pub fn score(
    candidate: &EntityKey,
    mention_normalized: &str,
    mention_type: &EntityTypeRef,
    graph_context: f64,
    embedding_sim: f64,
    config: &ResolutionConfig,
) -> f64 {
    // Hard type gate.
    if candidate.ty != *mention_type {
        return 0.0;
    }

    let exact = if candidate.normalized == mention_normalized {
        1.0
    } else {
        0.0
    };

    // N-gram Jaccard over 3-gram shingles (§7.7). Prefix-neutral, script-neutral.
    let cand_shingles = ngram::shingles(&candidate.normalized, 3);
    let ment_shingles = ngram::shingles(mention_normalized, 3);
    let j = ngram::jaccard(&cand_shingles, &ment_shingles);

    let emb_weight = config.w_embedding.weight(mention_type);

    let raw = config.w_exact * exact
        + config.w_ngram * j
        + config.w_graph * graph_context
        + emb_weight * embedding_sim;

    raw.clamp(0.0, 1.0)
}

// ─── Resolve ───────────────────────────────────────────────────────────────

/// Resolve a mention against a list of candidate entity keys.
///
/// `candidates` must already be filtered to the same space.
/// `graph_context` returns the context-overlap score [0, 1] for a candidate
/// entity (shared-neighbors fraction, §10.2).
/// `embedding_sim` returns the embedding-similarity score [0, 1] for a
/// candidate entity (§10.3). Both may return 0.0 when the signal is
/// unavailable — but the **caller** decides that, not this function.
///
/// Returns the decision: Link, New, or Candidate.
pub fn resolve(
    mention_normalized: &str,
    mention_type: &EntityTypeRef,
    candidates: &[EntityKey],
    graph_context: impl Fn(&EntityId) -> f64,
    embedding_sim: impl Fn(&EntityId) -> f64,
    config: &ResolutionConfig,
) -> Decision {
    // Score all candidates.
    let mut scored: Vec<(f64, &EntityKey)> = Vec::new();
    for c in candidates {
        let ctx = graph_context(&c.entity);
        let emb = embedding_sim(&c.entity);
        let s = score(c, mention_normalized, mention_type, ctx, emb, config);
        if s > 0.0 {
            scored.push((s, c));
        }
    }
    // Sort descending by score, then by entity id for determinism.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.entity.cmp(&b.1.entity))
    });

    match scored.first() {
        None => Decision::New {
            method: ResolutionMethod::New,
            score: 0.0,
        },
        Some(&(best, c)) if best >= config.tau_high => {
            let method = if c.normalized == mention_normalized {
                ResolutionMethod::ExactKey
            } else {
                ResolutionMethod::Lexical { score: best }
            };
            Decision::Link {
                entity: c.entity.clone(),
                method,
                score: best,
            }
        }
        Some(&(best, _c)) if best <= config.tau_low => Decision::New {
            method: ResolutionMethod::New,
            score: best,
        },
        Some(&(best, c)) => {
            // Between thresholds: new entity + merge candidate.
            Decision::Candidate {
                new_entity: String::new(), // caller assigns the new entity id
                existing: c.entity.clone(),
                score: best,
            }
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::KeyOrigin;

    fn make_key(entity: &str, normalized: &str, ty: &str) -> EntityKey {
        EntityKey {
            id: format!("k_{entity}_{normalized}"),
            space: "s1".into(),
            entity: entity.into(),
            ty: ty.into(),
            normalized: normalized.into(),
            surface: normalized.into(),
            origin: KeyOrigin::UserDeclared,
        }
    }

    #[test]
    fn exact_match_links() {
        let cands = vec![make_key("e1", "alice", "Person")];
        let dec = resolve(
            "alice",
            &"Person".to_string(),
            &cands,
            |_| 0.0,
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        match dec {
            Decision::Link {
                entity,
                method,
                score,
            } => {
                assert_eq!(entity, "e1");
                assert!(score >= 0.75);
                assert!(matches!(method, ResolutionMethod::ExactKey));
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn type_mismatch_rejected() {
        let cands = vec![make_key("e1", "alice", "Organization")];
        let dec = resolve(
            "alice",
            &"Person".to_string(),
            &cands,
            |_| 0.0,
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        assert!(matches!(dec, Decision::New { .. }));
    }

    #[test]
    fn no_candidates_is_new() {
        let dec = resolve(
            "alice",
            &"Person".to_string(),
            &[],
            |_| 0.0,
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        assert!(matches!(dec, Decision::New { .. }));
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize("Alice", &"Person".to_string()), "alice");
        assert_eq!(
            normalize("  Alice   Smith  ", &"Person".to_string()),
            "alice smith"
        );
    }

    #[test]
    fn low_similarity_is_new() {
        let cands = vec![make_key("e1", "zzzzzzzzz", "Person")];
        let dec = resolve(
            "alice",
            &"Person".to_string(),
            &cands,
            |_| 0.0,
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        assert!(matches!(dec, Decision::New { .. }));
    }

    // ── N-gram Jaccard behavior (§7.7, D30) ─────────────────────────────

    #[test]
    fn near_match_without_context_is_candidate() {
        // "alice" vs "alicia": Jaccard ≈ 0.36, below tau_high (0.75), above
        // tau_low (0.25) → Candidate, not Link.
        let cands = vec![make_key("e1", "alicia", "Person")];
        let dec = resolve(
            "alice",
            &"Person".to_string(),
            &cands,
            |_| 0.0,
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        assert!(
            matches!(dec, Decision::Candidate { .. }),
            "near match without context should be Candidate, got {dec:?}"
        );
    }

    #[test]
    fn near_match_with_context_links() {
        // Same near-match but with strong graph context → Link.
        let cands = vec![make_key("e1", "alicia", "Person")];
        let dec = resolve(
            "alice",
            &"Person".to_string(),
            &cands,
            |_| 1.0, // full context overlap
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        assert!(
            matches!(dec, Decision::Link { .. }),
            "near match with context should Link, got {dec:?}"
        );
    }

    #[test]
    fn prefix_sharing_does_not_inflate_score() {
        // D30: two Korean names sharing the surname 김 should NOT score as
        // highly similar. "김민수" vs "김서연" — shared prefix is the surname,
        // not evidence of identity.
        let cands = vec![make_key("e1", "김서연", "Person")];
        let dec = resolve(
            "김민수",
            &"Person".to_string(),
            &cands,
            |_| 0.0,
            |_| 0.0,
            &ResolutionConfig::default(),
        );
        // Without context, two different given names with a shared surname
        // should be New or Candidate, never Link.
        assert!(
            !matches!(dec, Decision::Link { .. }),
            "shared surname should not Link without context, got {dec:?}"
        );
    }

    // ── PerType ─────────────────────────────────────────────────────────

    #[test]
    fn pertype_default_and_override() {
        let mut pt = PerType::new(0.0);
        assert_eq!(pt.weight("Person"), 0.0);
        pt.set("Concept", 0.3);
        assert_eq!(pt.weight("Concept"), 0.3);
        assert_eq!(pt.weight("Person"), 0.0); // unmapped → default
    }
}
