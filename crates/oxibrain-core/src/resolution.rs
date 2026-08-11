//! Identity and resolution (DESIGN §8). M1: lexical only — exact key + Jaro-Winkler
//! + type gate + graph context. No embeddings (M3).

use crate::knowledge::{EntityId, EntityKey, EntityTypeRef, ResolutionMethod};

/// Configuration for resolution thresholds and scoring weights.
#[derive(Debug, Clone)]
pub struct ResolutionConfig {
    pub tau_high: f64,
    pub tau_low: f64,
    pub w_exact: f64,
    pub w_alias: f64,
    pub w_jw: f64,
    pub w_graph: f64,
}

impl Default for ResolutionConfig {
    fn default() -> Self {
        Self {
            tau_high: 0.85,
            tau_low: 0.55,
            w_exact: 1.0,
            w_alias: 0.8,
            w_jw: 0.6,
            w_graph: 0.4,
        }
    }
}

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

/// Normalize a surface form: NFKC, casefold, collapse whitespace.
/// Honorifics/suffixes per entity type are stripped in a future revision;
/// M1 does basic normalization only.
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

/// Compute the resolution score for a candidate.
///
/// score = type_gate × (w_exact·is_exact + w_jw·jw + w_graph·ctx)
/// type_gate = 0.0 if types disagree (hard reject), 1.0 if they match.
/// (`w_alias` is a reserved placeholder for M3 alias detection.)
pub fn score(
    candidate: &EntityKey,
    mention_normalized: &str,
    mention_type: &EntityTypeRef,
    graph_context: f64,
    config: &ResolutionConfig,
) -> f64 {
    // Hard type gate.
    if candidate.ty != *mention_type {
        return 0.0;
    }

    let exact = if candidate.normalized == mention_normalized { 1.0 } else { 0.0 };
    // M1: no alias detection here; w_alias term is a placeholder for M3.

    let jw = strsim::jaro_winkler(&candidate.normalized, mention_normalized);

    let raw = config.w_exact * exact
        + config.w_jw * jw
        + config.w_graph * graph_context;

    // Clamp to [0, 1].
    raw.clamp(0.0, 1.0)
}

/// Resolve a mention against a list of candidate entity keys.
///
/// `candidates` must already be filtered to the same space.
/// `graph_context` is a closure that returns the context-overlap score [0, 1]
/// for a given candidate entity (shared neighbors fraction).
///
/// Returns the decision: Link, New, or Candidate.
pub fn resolve(
    mention_normalized: &str,
    mention_type: &EntityTypeRef,
    candidates: &[EntityKey],
    graph_context: impl Fn(&EntityId) -> f64,
    config: &ResolutionConfig,
) -> Decision {
    // Score all candidates.
    let mut scored: Vec<(f64, &EntityKey)> = Vec::new();
    for c in candidates {
        let ctx = graph_context(&c.entity);
        let s = score(c, mention_normalized, mention_type, ctx, config);
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
        let dec = resolve("alice", &"Person".to_string(), &cands, |_| 0.0, &ResolutionConfig::default());
        match dec {
            Decision::Link { entity, method, score } => {
                assert_eq!(entity, "e1");
                assert!(score >= 0.85);
                assert!(matches!(method, ResolutionMethod::ExactKey));
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn type_mismatch_rejected() {
        let cands = vec![make_key("e1", "alice", "Organization")];
        let dec = resolve("alice", &"Person".to_string(), &cands, |_| 0.0, &ResolutionConfig::default());
        match dec {
            Decision::New { .. } => {}
            _ => panic!("expected New for type mismatch"),
        }
    }

    #[test]
    fn no_candidates_is_new() {
        let dec = resolve("alice", &"Person".to_string(), &[], |_| 0.0, &ResolutionConfig::default());
        assert!(matches!(dec, Decision::New { .. }));
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize("Alice", &"Person".to_string()), "alice");
        assert_eq!(normalize("  Alice   Smith  ", &"Person".to_string()), "alice smith");
    }

    #[test]
    fn low_similarity_is_new() {
        let cands = vec![make_key("e1", "zzzzzzzzz", "Person")];
        let dec = resolve("alice", &"Person".to_string(), &cands, |_| 0.0, &ResolutionConfig::default());
        assert!(matches!(dec, Decision::New { .. }));
    }
}