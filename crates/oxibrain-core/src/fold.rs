//! The temporal fold (DESIGN §6). A pure function that turns assertions into
//! current-slice beliefs. Operates at the (subject, predicate) GROUP level —
//! not per-statement — because Functional/Supersede predicates close intervals
//! across different objects (different StatementIds) sharing the same
//! subject+predicate (spec deviation D1).

use crate::confidence::{CalibrationTable, ConfidenceComponents, calibrate};
use crate::interval::{Interval, clip, merge_overlapping, overlaps};
use crate::knowledge::{
    Assertion, Belief, BeliefStatus, Polarity, Statement, StatementId, Support,
};
use crate::registry::{Cardinality, Invalidation, PredicateDef, Temporality};
use crate::types::TrustTier;
use oxibrain_ports::Timestamp;

/// A statement and its assertions — input to the fold for one (subject, predicate) group.
#[derive(Debug, Clone)]
pub struct StatementEntry {
    pub statement: Statement,
    pub assertions: Vec<Assertion>,
}

/// Per-statement view after transaction-time filtering and polarity partitioning.
struct VisibleStmt {
    stmt: Statement,
    affirm: Vec<Interval>,
    assertions: Vec<Assertion>, // visible ones, for support
}

/// Fold a (subject, predicate) group into current-slice beliefs.
///
/// `at` is the transaction-time cutoff: only assertions with
/// `recorded_at <= at && (retracted_at.is_none() || retracted_at > at)` are visible.
///
/// Pure function. Output is sorted by (statement_id, valid_from).
pub fn fold(
    def: &PredicateDef,
    group: &[StatementEntry],
    at: Timestamp,
    calibration: &CalibrationTable,
) -> Vec<Belief> {
    // ── Step 1: Filter by transaction time, partition by polarity per statement. ──

    let mut visible: Vec<VisibleStmt> = Vec::new();
    for entry in group {
        let vis: Vec<&Assertion> = entry
            .assertions
            .iter()
            .filter(|a| {
                a.recorded_at <= at && (a.retracted_at.is_none() || a.retracted_at.unwrap() > at)
            })
            .collect();
        if vis.is_empty() {
            continue;
        }

        let mut affirm: Vec<Interval> = vis
            .iter()
            .filter(|a| a.polarity == Polarity::Affirm)
            .map(|a| Interval::new(a.claimed_from, a.claimed_to))
            .collect();
        let deny: Vec<Interval> = vis
            .iter()
            .filter(|a| a.polarity == Polarity::Deny)
            .map(|a| Interval::new(a.claimed_from, a.claimed_to))
            .collect();

        // Merge overlapping affirming intervals.
        merge_overlapping(&mut affirm);

        // Apply denials: clip affirming intervals.
        for d in &deny {
            affirm = clip(&affirm, d);
        }

        visible.push(VisibleStmt {
            stmt: entry.statement.clone(),
            affirm,
            assertions: vis.into_iter().cloned().collect(),
        });
    }

    if visible.is_empty() {
        return Vec::new();
    }

    // ── Step 2: Apply cross-object rules. ──
    let beliefs = match (def.cardinality, def.invalidation, def.temporality) {
        // MultiValued: per-statement, no cross-object effect.
        (Cardinality::MultiValued, _, _) => fold_independent(&visible, calibration),

        // Functional + Static → contradiction on 2+ overlapping objects.
        (Cardinality::Functional, _, Temporality::Static) => {
            fold_contradiction(&visible, calibration)
        }

        // Functional + Supersede + Interval/Point → newer supersedes older.
        (Cardinality::Functional, Invalidation::Supersede, _) => {
            fold_supersede(&visible, calibration)
        }

        // Functional + ExplicitOnly → both stay Active (no auto-close).
        (Cardinality::Functional, Invalidation::ExplicitOnly, _) => {
            fold_independent(&visible, calibration)
        }

        // Functional + Coexist → treat as MultiValued.
        (Cardinality::Functional, Invalidation::Coexist, _) => {
            fold_independent(&visible, calibration)
        }
    };

    // ── Step 3: Sort output by (statement_id, valid_from). ──
    let mut beliefs = beliefs;
    beliefs.sort_by(|a, b| (&a.statement, a.valid_from).cmp(&(&b.statement, b.valid_from)));
    beliefs
}

/// Compute belief confidence from supporting assertions (DESIGN §6.5).
/// Pure function of the assertion set — deterministic.
fn belief_confidence(
    assertions: &[Assertion],
    support: &Support,
    calibration: &CalibrationTable,
) -> f32 {
    // Manual declarations (no extractor) bypass at 1.0.
    let is_declaration = assertions.iter().all(|a| a.extractor.is_none());
    if is_declaration {
        return 1.0;
    }

    // Raw: max assertion confidence (strongest evidence in the interval).
    let raw = assertions
        .iter()
        .map(|a| a.confidence)
        .fold(0.0_f32, f32::max);

    // Calibrated: per-extractor multiplier from eval harness (default 0.8).
    let extractor_id = assertions
        .iter()
        .filter_map(|a| a.extractor.as_deref())
        .next()
        .unwrap_or("unknown");
    let calibrated = calibrate(extractor_id, calibration);

    // Corroboration: saturating in distinct supporting episodes.
    let n = support.distinct_episodes.max(1) as f32;
    let corroboration = (1.0 - (-0.3 * n).exp()).clamp(0.5, 1.0);

    // Trust: weighted by episode trust tier.
    let trust = if support.trust_weights.is_empty() {
        1.0
    } else {
        let total: u32 = support.trust_weights.iter().map(|(_, c)| *c).sum();
        if total == 0 {
            1.0
        } else {
            let weighted: f32 = support
                .trust_weights
                .iter()
                .map(|(tier, count)| {
                    let w = match tier {
                        TrustTier::Trusted => 1.0,
                        TrustTier::SemiTrusted => 0.7,
                        TrustTier::Untrusted => 0.3,
                    };
                    w * *count as f32
                })
                .sum();
            (weighted / total as f32).clamp(0.3, 1.0)
        }
    };

    // Recency: fixed at 1.0 for v1 — needs reference time parameter.
    let recency = 1.0;

    ConfidenceComponents {
        raw,
        calibrated,
        corroboration,
        trust,
        recency,
    }
    .combine()
}

/// Per-statement fold: each object's affirming intervals become Active beliefs.
fn fold_independent(visible: &[VisibleStmt], calibration: &CalibrationTable) -> Vec<Belief> {
    let mut beliefs = Vec::new();
    for vs in visible {
        let support = compute_support(&vs.assertions);
        let conf = belief_confidence(&vs.assertions, &support, calibration);
        for iv in &vs.affirm {
            beliefs.push(Belief {
                statement: vs.stmt.id.clone(),
                valid_from: iv.start,
                valid_to: iv.end,
                support: support.clone(),
                confidence: conf,
                status: BeliefStatus::Active,
            });
        }
    }
    beliefs
}

/// Contradiction fold: for Static+Functional, all overlapping objects are Contradicted.
fn fold_contradiction(visible: &[VisibleStmt], calibration: &CalibrationTable) -> Vec<Belief> {
    // If only one object has affirming intervals, it's Active (no contradiction).
    let affirming: Vec<&VisibleStmt> = visible.iter().filter(|vs| !vs.affirm.is_empty()).collect();
    if affirming.len() <= 1 {
        return fold_independent(visible, calibration);
    }

    // Check for pairwise overlaps across different statements.
    // An object is Contradicted if ANY of its intervals overlaps with another object's interval.
    let mut contradicted: Vec<&str> = Vec::new(); // statement ids
    for i in 0..affirming.len() {
        for j in (i + 1)..affirming.len() {
            let a = &affirming[i];
            let b = &affirming[j];
            let overlap = a
                .affirm
                .iter()
                .any(|ai| b.affirm.iter().any(|bi| overlaps(ai, bi)));
            if overlap {
                if !contradicted.contains(&a.stmt.id.as_str()) {
                    contradicted.push(&a.stmt.id);
                }
                if !contradicted.contains(&b.stmt.id.as_str()) {
                    contradicted.push(&b.stmt.id);
                }
            }
        }
    }

    let mut beliefs = Vec::new();
    for vs in visible {
        let support = compute_support(&vs.assertions);
        let conf = belief_confidence(&vs.assertions, &support, calibration);
        let is_contradicted = contradicted.contains(&vs.stmt.id.as_str());
        for iv in &vs.affirm {
            beliefs.push(Belief {
                statement: vs.stmt.id.clone(),
                valid_from: iv.start,
                valid_to: iv.end,
                support: support.clone(),
                confidence: conf,
                status: if is_contradicted {
                    BeliefStatus::Contradicted
                } else {
                    BeliefStatus::Active
                },
            });
        }
    }
    beliefs
}

/// Supersession fold: for Functional/Supersede/Interval, newer objects close older ones.
fn fold_supersede(visible: &[VisibleStmt], calibration: &CalibrationTable) -> Vec<Belief> {
    // Collect (statement_id, interval) pairs across all objects.
    let mut all: Vec<(StatementId, Interval)> = Vec::new();
    for vs in visible {
        for iv in &vs.affirm {
            all.push((vs.stmt.id.clone(), *iv));
        }
    }

    // Sort by (start, statement_id) for deterministic processing.
    all.sort_by(|a, b| (&a.1.start, &a.0).cmp(&(&b.1.start, &b.0)));

    let mut beliefs: Vec<Belief> = Vec::new();
    struct Active {
        stmt: StatementId,
        start: Timestamp,
        end: Timestamp,
    }

    let mut current: Option<Active> = None;

    for (stmt_id, iv) in &all {
        let vs = visible
            .iter()
            .find(|vs| &vs.stmt.id == stmt_id)
            .expect("statement exists in group");
        let support = compute_support(&vs.assertions);
        let conf = belief_confidence(&vs.assertions, &support, calibration);

        match &current {
            None => {
                beliefs.push(Belief {
                    statement: stmt_id.clone(),
                    valid_from: iv.start,
                    valid_to: iv.end,
                    support,
                    confidence: conf,
                    status: BeliefStatus::Active,
                });
                current = Some(Active {
                    stmt: stmt_id.clone(),
                    start: iv.start,
                    end: iv.end,
                });
            }
            Some(cur) if cur.stmt == *stmt_id => {
                beliefs.push(Belief {
                    statement: stmt_id.clone(),
                    valid_from: iv.start,
                    valid_to: iv.end,
                    support,
                    confidence: conf,
                    status: BeliefStatus::Active,
                });
                if iv.end > cur.end {
                    current = Some(Active {
                        stmt: stmt_id.clone(),
                        start: cur.start,
                        end: iv.end,
                    });
                }
            }
            Some(cur) => {
                if iv.start == cur.start {
                    if let Some(last) = beliefs.last_mut() {
                        if last.statement == cur.stmt && last.status == BeliefStatus::Active {
                            last.status = BeliefStatus::Contradicted;
                        }
                    }
                    beliefs.push(Belief {
                        statement: stmt_id.clone(),
                        valid_from: iv.start,
                        valid_to: iv.end,
                        support,
                        confidence: conf,
                        status: BeliefStatus::Contradicted,
                    });
                } else {
                    if let Some(last) = beliefs.last_mut() {
                        if last.statement == cur.stmt
                            && last.status == BeliefStatus::Active
                            && last.valid_to >= iv.start
                        {
                            last.valid_to = Timestamp(iv.start.millis() - 1);
                            last.status = BeliefStatus::Superseded;
                        }
                    }
                    beliefs.push(Belief {
                        statement: stmt_id.clone(),
                        valid_from: iv.start,
                        valid_to: iv.end,
                        support,
                        confidence: conf,
                        status: BeliefStatus::Active,
                    });
                }
                current = Some(Active {
                    stmt: stmt_id.clone(),
                    start: iv.start,
                    end: iv.end,
                });
            }
        }
    }

    beliefs
}

/// Compute support from visible assertions.
fn compute_support(assertions: &[Assertion]) -> Support {
    use std::collections::HashSet;

    let affirm_count = assertions
        .iter()
        .filter(|a| a.polarity == Polarity::Affirm)
        .count() as u32;
    let deny_count = assertions
        .iter()
        .filter(|a| a.polarity == Polarity::Deny)
        .count() as u32;

    let distinct_episodes: HashSet<&str> = assertions.iter().map(|a| a.episode.as_str()).collect();

    // In M1, all declarations are Trusted by default (no trust tier system until M4).
    // All distinct episodes count as Trusted. Sorted deterministically (single entry).
    let trust_weights = if distinct_episodes.is_empty() {
        Vec::new()
    } else {
        vec![(TrustTier::Trusted, distinct_episodes.len() as u32)]
    };

    Support {
        affirm_count,
        deny_count,
        distinct_episodes: distinct_episodes.len() as u32,
        trust_weights,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::CalibrationTable;
    use crate::knowledge::{Object, Polarity, Statement};
    use crate::registry::{Cardinality, Invalidation, ObjectKind, PredicateDef, Temporality};
    use oxibrain_ports::{TIME_MAX, TIME_MIN, Timestamp};

    fn ts(m: i64) -> Timestamp {
        Timestamp(m)
    }

    fn make_assertion(
        stmt: &str,
        episode: &str,
        polarity: Polarity,
        from: Timestamp,
        to: Timestamp,
    ) -> Assertion {
        Assertion {
            id: format!("a_{stmt}_{episode}"),
            statement: stmt.into(),
            episode: episode.into(),
            extractor: None,
            polarity,
            claimed_from: from,
            claimed_to: to,
            confidence: 1.0,
            recorded_at: ts(1),
            retracted_at: None,
        }
    }

    fn make_stmt(id: &str, subj: &str, pred: &str, obj_id: &str) -> Statement {
        Statement {
            id: id.into(),
            space: "s1".into(),
            subject: subj.into(),
            predicate: pred.into(),
            object: Object::Entity(obj_id.into()),
        }
    }

    fn def_employed() -> PredicateDef {
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
            profile_relevant: false,
        }
    }

    fn def_born_in() -> PredicateDef {
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
            profile_relevant: false,
        }
    }

    fn def_works_on() -> PredicateDef {
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
            profile_relevant: false,
        }
    }

    // ── Basic fold: single assertion → one Active belief. ──
    #[test]
    fn single_affirm_is_active() {
        let stmt = make_stmt("st1", "e1", "employed_by", "acme");
        let group = vec![StatementEntry {
            statement: stmt,
            assertions: vec![make_assertion(
                "st1",
                "ep1",
                Polarity::Affirm,
                ts(100),
                TIME_MAX,
            )],
        }];
        let beliefs = fold(
            &def_employed(),
            &group,
            TIME_MAX,
            &CalibrationTable::default(),
        );
        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0].status, BeliefStatus::Active);
        assert_eq!(beliefs[0].valid_from, ts(100));
    }

    // ── Supersession: two employers, second supersedes first. ──
    #[test]
    fn supersession_closes_previous() {
        let stmt_a = make_stmt("st_a", "e1", "employed_by", "acme");
        let stmt_b = make_stmt("st_b", "e1", "employed_by", "globex");
        let group = vec![
            StatementEntry {
                statement: stmt_a,
                assertions: vec![make_assertion(
                    "st_a",
                    "ep1",
                    Polarity::Affirm,
                    ts(100),
                    TIME_MAX,
                )],
            },
            StatementEntry {
                statement: stmt_b,
                assertions: vec![make_assertion(
                    "st_b",
                    "ep2",
                    Polarity::Affirm,
                    ts(200),
                    TIME_MAX,
                )],
            },
        ];
        let beliefs = fold(
            &def_employed(),
            &group,
            TIME_MAX,
            &CalibrationTable::default(),
        );
        // Acme: [100, 199] Superseded. Globex: [200, MAX] Active.
        let acme = beliefs
            .iter()
            .find(|b| b.statement == "st_a")
            .expect("acme belief");
        let globex = beliefs
            .iter()
            .find(|b| b.statement == "st_b")
            .expect("globex belief");
        assert_eq!(acme.status, BeliefStatus::Superseded);
        assert_eq!(acme.valid_to, ts(199));
        assert_eq!(globex.status, BeliefStatus::Active);
        assert_eq!(globex.valid_from, ts(200));
    }

    // ── Contradiction: two birthplaces for Static predicate. ──
    #[test]
    fn static_two_values_contradicted() {
        let stmt_a = make_stmt("st_a", "e1", "born_in", "seoul");
        let stmt_b = make_stmt("st_b", "e1", "born_in", "busan");
        let group = vec![
            StatementEntry {
                statement: stmt_a,
                assertions: vec![make_assertion(
                    "st_a",
                    "ep1",
                    Polarity::Affirm,
                    TIME_MIN,
                    TIME_MAX,
                )],
            },
            StatementEntry {
                statement: stmt_b,
                assertions: vec![make_assertion(
                    "st_b",
                    "ep2",
                    Polarity::Affirm,
                    TIME_MIN,
                    TIME_MAX,
                )],
            },
        ];
        let beliefs = fold(
            &def_born_in(),
            &group,
            TIME_MAX,
            &CalibrationTable::default(),
        );
        assert_eq!(beliefs.len(), 2);
        assert!(
            beliefs
                .iter()
                .all(|b| b.status == BeliefStatus::Contradicted)
        );
    }

    // ── Coexist: two projects for MultiValued predicate. ──
    #[test]
    fn multivalued_coexist() {
        let stmt_a = make_stmt("st_a", "e1", "works_on", "px");
        let stmt_b = make_stmt("st_b", "e1", "works_on", "py");
        let group = vec![
            StatementEntry {
                statement: stmt_a,
                assertions: vec![make_assertion(
                    "st_a",
                    "ep1",
                    Polarity::Affirm,
                    ts(100),
                    TIME_MAX,
                )],
            },
            StatementEntry {
                statement: stmt_b,
                assertions: vec![make_assertion(
                    "st_b",
                    "ep2",
                    Polarity::Affirm,
                    ts(100),
                    TIME_MAX,
                )],
            },
        ];
        let beliefs = fold(
            &def_works_on(),
            &group,
            TIME_MAX,
            &CalibrationTable::default(),
        );
        assert_eq!(beliefs.len(), 2);
        assert!(beliefs.iter().all(|b| b.status == BeliefStatus::Active));
    }

    // ── Denial clips affirming interval. ──
    #[test]
    fn denial_clips() {
        let stmt = make_stmt("st1", "e1", "works_on", "px");
        let group = vec![StatementEntry {
            statement: stmt,
            assertions: vec![
                make_assertion("st1", "ep1", Polarity::Affirm, ts(100), ts(500)),
                Assertion {
                    id: "deny1".into(),
                    statement: "st1".into(),
                    episode: "ep2".into(),
                    extractor: None,
                    polarity: Polarity::Deny,
                    claimed_from: ts(200),
                    claimed_to: ts(300),
                    confidence: 1.0,
                    recorded_at: ts(2),
                    retracted_at: None,
                },
            ],
        }];
        let beliefs = fold(
            &def_works_on(),
            &group,
            TIME_MAX,
            &CalibrationTable::default(),
        );
        // Affirming [100, 500] clipped by denial [200, 300] → [100, 199] and [301, 500].
        assert_eq!(beliefs.len(), 2);
        assert_eq!(beliefs[0].valid_from, ts(100));
        assert_eq!(beliefs[0].valid_to, ts(199));
        assert_eq!(beliefs[1].valid_from, ts(301));
        assert_eq!(beliefs[1].valid_to, ts(500));
    }

    // ── Retracted assertion is filtered out. ──
    #[test]
    fn retracted_assertion_invisible() {
        let stmt = make_stmt("st1", "e1", "employed_by", "acme");
        let group = vec![StatementEntry {
            statement: stmt,
            assertions: vec![Assertion {
                id: "a1".into(),
                statement: "st1".into(),
                episode: "ep1".into(),
                extractor: None,
                polarity: Polarity::Affirm,
                claimed_from: ts(100),
                claimed_to: TIME_MAX,
                confidence: 1.0,
                recorded_at: ts(1),
                retracted_at: Some(ts(5)), // retracted before `at`
            }],
        }];
        let beliefs = fold(
            &def_employed(),
            &group,
            ts(10),
            &CalibrationTable::default(),
        );
        assert!(
            beliefs.is_empty(),
            "retracted assertion should produce no belief"
        );
    }

    // ── Output is sorted by (statement_id, valid_from). ──
    #[test]
    fn output_sorted() {
        let stmt_a = make_stmt("st_a", "e1", "works_on", "px");
        let stmt_b = make_stmt("st_b", "e1", "works_on", "py");
        let group = vec![
            StatementEntry {
                statement: stmt_b,
                assertions: vec![make_assertion(
                    "st_b",
                    "ep2",
                    Polarity::Affirm,
                    ts(200),
                    TIME_MAX,
                )],
            },
            StatementEntry {
                statement: stmt_a,
                assertions: vec![make_assertion(
                    "st_a",
                    "ep1",
                    Polarity::Affirm,
                    ts(100),
                    TIME_MAX,
                )],
            },
        ];
        let beliefs = fold(
            &def_works_on(),
            &group,
            TIME_MAX,
            &CalibrationTable::default(),
        );
        assert_eq!(beliefs[0].statement, "st_a");
        assert_eq!(beliefs[1].statement, "st_b");
    }

    // ── Empty group → empty output. ──
    #[test]
    fn empty_group() {
        let beliefs = fold(&def_employed(), &[], TIME_MAX, &CalibrationTable::default());
        assert!(beliefs.is_empty());
    }
}
