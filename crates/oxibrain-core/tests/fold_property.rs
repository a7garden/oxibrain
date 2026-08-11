//! Property tests for the temporal fold (DESIGN §6).
//!
//! These complement the example-based tests in `fold.rs` with invariant checks
//! over randomly generated assertion groups. Required by AGENTS.md:
//! "Property tests for the temporal fold".

use oxibrain_core::{
    Assertion, BeliefStatus, Cardinality, Interval, Invalidation, Object, ObjectKind, Polarity,
    PredicateDef, Statement, StatementEntry, Temporality, fold, overlaps,
};
use oxibrain_ports::{TIME_MAX, Timestamp};
use proptest::prelude::*;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn ts(m: i64) -> Timestamp {
    Timestamp(m)
}

fn make_stmt(id: &str, obj: &str) -> Statement {
    Statement {
        id: id.into(),
        space: "s1".into(),
        subject: "e1".into(),
        predicate: "p".into(),
        object: Object::Entity(obj.into()),
    }
}

fn make_assertion(stmt: &str, ep: &str, polarity: Polarity, from: i64, to: i64) -> Assertion {
    Assertion {
        id: format!("a_{stmt}_{ep}"),
        statement: stmt.into(),
        episode: ep.into(),
        extractor: None,
        polarity,
        claimed_from: ts(from),
        claimed_to: ts(to),
        confidence: 1.0,
        recorded_at: ts(1),
        retracted_at: None,
    }
}

fn def_supersede() -> PredicateDef {
    PredicateDef {
        name: "employed_by".into(),
        object_kind: ObjectKind::Entity("Organization".into()),
        subject_types: vec!["Person".into()],
        cardinality: Cardinality::Functional,
        temporality: Temporality::Interval,
        invalidation: Invalidation::Supersede,
        symmetric: false,
        inverse_of: None,
        description: "".into(),
        examples: vec![],
        deprecated_by: None,
    }
}

fn def_static() -> PredicateDef {
    PredicateDef {
        name: "born_in".into(),
        object_kind: ObjectKind::Entity("Place".into()),
        subject_types: vec!["Person".into()],
        cardinality: Cardinality::Functional,
        temporality: Temporality::Static,
        invalidation: Invalidation::Supersede,
        symmetric: false,
        inverse_of: None,
        description: "".into(),
        examples: vec![],
        deprecated_by: None,
    }
}

fn def_multivalued() -> PredicateDef {
    PredicateDef {
        name: "works_on".into(),
        object_kind: ObjectKind::Entity("Project".into()),
        subject_types: vec!["Person".into()],
        cardinality: Cardinality::MultiValued,
        temporality: Temporality::Interval,
        invalidation: Invalidation::Coexist,
        symmetric: false,
        inverse_of: None,
        description: "".into(),
        examples: vec![],
        deprecated_by: None,
    }
}

// ── Strategies ───────────────────────────────────────────────────────────────

/// Generate a valid interval (from, to) with from <= to.
fn arb_interval() -> impl Strategy<Value = (i64, i64)> {
    (100i64..4000, 0i64..2000).prop_map(|(from, len)| (from, from + len))
}

/// Randomly pick one of the three fold-relevant predicate defs.
fn arb_def() -> impl Strategy<Value = PredicateDef> {
    prop_oneof![
        Just(def_supersede()),
        Just(def_static()),
        Just(def_multivalued()),
    ]
}

/// Generate a group of 0–5 statements, each with one affirming assertion.
fn arb_simple_group() -> impl Strategy<Value = Vec<StatementEntry>> {
    prop::collection::vec(arb_interval(), 0..6).prop_map(|ivs| {
        ivs.into_iter()
            .enumerate()
            .map(|(i, (from, to))| {
                let sid = format!("st_{i}");
                StatementEntry {
                    statement: make_stmt(&sid, &format!("obj_{i}")),
                    assertions: vec![make_assertion(&sid, "ep1", Polarity::Affirm, from, to)],
                }
            })
            .collect()
    })
}

/// Generate a single-statement group with affirm + deny assertions.
/// Returns (group, denial_intervals) so the test can check coverage.
fn arb_denial_group() -> impl Strategy<Value = (Vec<StatementEntry>, Vec<(i64, i64)>)> {
    (
        prop::collection::vec(arb_interval(), 1..4),
        prop::collection::vec(arb_interval(), 0..3),
    )
        .prop_map(|(affirms, denies)| {
            let mut assertions: Vec<Assertion> = affirms
                .iter()
                .enumerate()
                .map(|(i, &(from, to))| {
                    make_assertion("st_0", &format!("a{i}"), Polarity::Affirm, from, to)
                })
                .collect();
            assertions.extend(denies.iter().enumerate().map(|(i, &(from, to))| {
                make_assertion("st_0", &format!("d{i}"), Polarity::Deny, from, to)
            }));
            let group = vec![StatementEntry {
                statement: make_stmt("st_0", "obj_0"),
                assertions,
            }];
            (group, denies)
        })
}

// ── Universal properties (hold for all fold modes) ──────────────────────────

proptest! {
    /// Output is always sorted by (statement_id, valid_from).
    #[test]
    fn output_always_sorted(
        def in arb_def(),
        group in arb_simple_group(),
    ) {
        let beliefs = fold(&def, &group, TIME_MAX);
        for w in beliefs.windows(2) {
            let a = (&w[0].statement, w[0].valid_from);
            let b = (&w[1].statement, w[1].valid_from);
            prop_assert!(a <= b, "beliefs must be sorted by (statement_id, valid_from)");
        }
    }

    /// All belief intervals are well-formed: valid_from <= valid_to.
    #[test]
    fn intervals_well_formed(
        def in arb_def(),
        group in arb_simple_group(),
    ) {
        let beliefs = fold(&def, &group, TIME_MAX);
        for b in &beliefs {
            prop_assert!(
                b.valid_from <= b.valid_to,
                "valid_from ({:?}) must not exceed valid_to ({:?})",
                b.valid_from,
                b.valid_to,
            );
        }
    }

    /// Denial intervals never appear in the output: no belief interval
    /// overlaps with any denial from the same statement.
    #[test]
    fn denial_eliminated_from_output(
        (group, denies) in arb_denial_group(),
    ) {
        let beliefs = fold(&def_multivalued(), &group, TIME_MAX);
        for b in &beliefs {
            for &(d_from, d_to) in &denies {
                let b_iv = Interval::new(b.valid_from, b.valid_to);
                let d_iv = Interval::new(ts(d_from), ts(d_to));
                prop_assert!(
                    !overlaps(&b_iv, &d_iv),
                    "belief [{:?}, {:?}] must not overlap denial [{:?}, {:?}]",
                    b.valid_from, b.valid_to, d_iv.start, d_iv.end,
                );
            }
        }
    }
}

// ── Mode-specific properties ────────────────────────────────────────────────

proptest! {
    /// Functional + Supersede: at most one Active belief per point in time.
    /// No two Active beliefs from different statements may overlap.
    #[test]
    fn supersede_no_overlapping_active(
        group in arb_simple_group(),
    ) {
        let beliefs = fold(&def_supersede(), &group, TIME_MAX);
        let active: Vec<_> = beliefs.iter().filter(|b| b.status == BeliefStatus::Active).collect();
        for i in 0..active.len() {
            for j in (i + 1)..active.len() {
                if active[i].statement != active[j].statement {
                    let a = Interval::new(active[i].valid_from, active[i].valid_to);
                    let b = Interval::new(active[j].valid_from, active[j].valid_to);
                    prop_assert!(
                        !overlaps(&a, &b),
                        "Active beliefs from different statements must not overlap: \
                         {} [{:?}, {:?}] vs {} [{:?}, {:?}]",
                        active[i].statement, a.start, a.end,
                        active[j].statement, b.start, b.end,
                    );
                }
            }
        }
    }

    /// Functional + Static: at most one Active belief per point in time.
    /// Overlapping objects are Contradicted, not Active.
    #[test]
    fn contradiction_no_overlapping_active(
        group in arb_simple_group(),
    ) {
        let beliefs = fold(&def_static(), &group, TIME_MAX);
        let active: Vec<_> = beliefs.iter().filter(|b| b.status == BeliefStatus::Active).collect();
        for i in 0..active.len() {
            for j in (i + 1)..active.len() {
                if active[i].statement != active[j].statement {
                    let a = Interval::new(active[i].valid_from, active[i].valid_to);
                    let b = Interval::new(active[j].valid_from, active[j].valid_to);
                    prop_assert!(
                        !overlaps(&a, &b),
                        "Active beliefs from different statements must not overlap (Static): \
                         {} [{:?}, {:?}] vs {} [{:?}, {:?}]",
                        active[i].statement, a.start, a.end,
                        active[j].statement, b.start, b.end,
                    );
                }
            }
        }
    }

    /// MultiValued: every belief is Active. No supersession or contradiction.
    #[test]
    fn multivalued_all_active(
        group in arb_simple_group(),
    ) {
        let beliefs = fold(&def_multivalued(), &group, TIME_MAX);
        for b in &beliefs {
            prop_assert_eq!(
                b.status,
                BeliefStatus::Active,
                "MultiValued fold must produce only Active beliefs"
            );
        }
    }
}

// ── Edge cases ───────────────────────────────────────────────────────────────

#[test]
fn empty_group_empty_output() {
    assert!(fold(&def_supersede(), &[], TIME_MAX).is_empty());
    assert!(fold(&def_static(), &[], TIME_MAX).is_empty());
    assert!(fold(&def_multivalued(), &[], TIME_MAX).is_empty());
}

#[test]
fn single_affirm_produces_one_active() {
    let group = vec![StatementEntry {
        statement: make_stmt("st_0", "acme"),
        assertions: vec![make_assertion("st_0", "ep1", Polarity::Affirm, 100, 500)],
    }];
    for def in [def_supersede(), def_static(), def_multivalued()] {
        let beliefs = fold(&def, &group, TIME_MAX);
        assert_eq!(beliefs.len(), 1, "single affirm → exactly one belief");
        assert_eq!(beliefs[0].status, BeliefStatus::Active);
        assert_eq!(beliefs[0].valid_from, ts(100));
        assert_eq!(beliefs[0].valid_to, ts(500));
    }
}
