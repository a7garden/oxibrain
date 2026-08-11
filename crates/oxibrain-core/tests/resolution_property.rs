//! Property tests for identity and resolution (DESIGN §8).
//!
//! These complement the example-based tests in `resolution.rs` with invariant
//! checks over randomly generated candidates. Required by AGENTS.md:
//! "Property tests for ... resolution decisions".

use oxibrain_core::resolution::{Decision, ResolutionConfig, normalize, resolve, score};
use oxibrain_core::{EntityKey, KeyOrigin};
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────────────

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

// ── Strategies ───────────────────────────────────────────────────────────────

/// Generate a random entity name from lowercase ASCII (3–8 chars).
fn arb_name() -> impl Strategy<Value = String> {
    "[a-z]{3,8}"
}

/// Generate a random entity type from a small set.
fn arb_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("Person".to_string()),
        Just("Organization".to_string()),
        Just("Place".to_string()),
    ]
}

/// Generate a random graph-context score in [0, 1].
fn arb_context() -> impl Strategy<Value = f64> {
    0.0f64..=1.0
}

// ── Score properties ─────────────────────────────────────────────────────────

proptest! {
    /// Score is always in [0, 1], regardless of inputs.
    #[test]
    fn score_in_bounds(
        name in arb_name(),
        ty in arb_type(),
        cand_name in arb_name(),
        cand_ty in arb_type(),
        ctx in arb_context(),
    ) {
        let candidate = make_key("e1", &cand_name, &cand_ty);
        let s = score(&candidate, &name, &ty, ctx, &ResolutionConfig::default());
        prop_assert!((0.0..=1.0).contains(&s), "score {s} must be in [0, 1]");
    }

    /// Type mismatch always yields score 0 (hard gate).
    #[test]
    fn type_mismatch_zero_score(
        name in arb_name(),
        cand_name in arb_name(),
        ctx in arb_context(),
    ) {
        let candidate = make_key("e1", &cand_name, "Organization");
        let s = score(&candidate, &name, &"Person".to_string(), ctx, &ResolutionConfig::default());
        prop_assert_eq!(s, 0.0, "type mismatch must gate to 0");
    }

    /// Exact normalized match with matching type always links (score ≥ tau_high).
    #[test]
    fn exact_match_always_links(
        name in arb_name(),
        ty in arb_type(),
        ctx in arb_context(),
    ) {
        let candidate = make_key("e1", &name, &ty);
        let dec = resolve(
            &name,
            &ty,
            &[candidate],
            move |_| ctx,
            &ResolutionConfig::default(),
        );
        match dec {
            Decision::Link { score, .. } => {
                prop_assert!(score >= 0.85, "exact match score {score} must reach tau_high");
            }
            _ => prop_assert!(false, "exact match must Link, got {dec:?}"),
        }
    }

    /// When multiple candidates exist, resolve always picks one with the
    /// highest score (or reports a tie deterministically by entity id).
    #[test]
    fn resolve_picks_highest_score(
        names in prop::collection::vec(arb_name(), 1..5),
        ty in arb_type(),
        ctx in arb_context(),
    ) {
        let candidates: Vec<EntityKey> = names
            .iter()
            .enumerate()
            .map(|(i, n)| make_key(&format!("e{i}"), n, &ty))
            .collect();
        let mention = &names[0];
        let dec = resolve(
            mention,
            &ty,
            &candidates,
            |_| ctx,
            &ResolutionConfig::default(),
        );

        // Compute the best possible score independently.
        let best = candidates
            .iter()
            .map(|c| score(c, mention, &ty, ctx, &ResolutionConfig::default()))
            .fold(0.0_f64, f64::max);

        match &dec {
            Decision::Link { score, entity, .. } => {
                prop_assert!(*score >= best - 1e-9, "Link score {score} must equal best {best}");
                // The linked entity must be a real candidate.
                prop_assert!(candidates.iter().any(|c| &c.entity == entity));
            }
            Decision::New { score, .. } => {
                prop_assert!(*score <= 0.55, "New score {score} must be ≤ tau_low");
            }
            Decision::Candidate { score, .. } => {
                prop_assert!((0.55..0.85).contains(score), "Candidate score in ambiguity band");
            }
        }
    }
}

// ── Normalize properties ─────────────────────────────────────────────────────

proptest! {
    /// normalize is idempotent: normalize(normalize(x)) == normalize(x).
    #[test]
    fn normalize_idempotent(
        s in "[A-Za-z ]{1,20}"
    ) {
        let once = normalize(&s, &"Person".to_string());
        let twice = normalize(&once, &"Person".to_string());
        prop_assert_eq!(once, twice, "normalize must be idempotent");
    }

    /// normalize produces lowercase output with collapsed whitespace.
    #[test]
    fn normalize_lowercase_collapsed(
        s in "[A-Za-z \t]{1,20}"
    ) {
        let n = normalize(&s, &"Person".to_string());
        // No uppercase.
        prop_assert!(
            !n.chars().any(|c| c.is_uppercase()),
            "normalized form must be lowercase: '{n}'"
        );
        // No runs of multiple spaces.
        prop_assert!(
            !n.contains("  "),
            "normalized form must have collapsed whitespace: '{n}'"
        );
    }
}

// ── Edge cases ───────────────────────────────────────────────────────────────

#[test]
fn no_candidates_is_new() {
    let dec = resolve(
        "alice",
        &"Person".to_string(),
        &[],
        |_| 0.0,
        &ResolutionConfig::default(),
    );
    assert!(matches!(dec, Decision::New { .. }));
}

#[test]
fn resolve_deterministic_same_input() {
    let candidates = vec![
        make_key("e1", "alice", "Person"),
        make_key("e2", "alicia", "Person"),
        make_key("e3", "bob", "Person"),
    ];
    let d1 = resolve(
        "alice",
        &"Person".to_string(),
        &candidates,
        |_| 0.3,
        &ResolutionConfig::default(),
    );
    let d2 = resolve(
        "alice",
        &"Person".to_string(),
        &candidates,
        |_| 0.3,
        &ResolutionConfig::default(),
    );
    // Decisions must be identical.
    match (&d1, &d2) {
        (
            Decision::Link {
                entity: e1,
                score: s1,
                ..
            },
            Decision::Link {
                entity: e2,
                score: s2,
                ..
            },
        ) => {
            assert_eq!(e1, e2);
            assert!((s1 - s2).abs() < 1e-9);
        }
        (Decision::New { score: s1, .. }, Decision::New { score: s2, .. }) => {
            assert!((s1 - s2).abs() < 1e-9);
        }
        (
            Decision::Candidate {
                score: s1,
                existing: ex1,
                ..
            },
            Decision::Candidate {
                score: s2,
                existing: ex2,
                ..
            },
        ) => {
            assert_eq!(ex1, ex2);
            assert!((s1 - s2).abs() < 1e-9);
        }
        _ => panic!("decisions must match: {d1:?} vs {d2:?}"),
    }
}
