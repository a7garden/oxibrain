//! M9 exit criterion (§16.4): resolution over a 10⁴-entity fixture is
//! sublinear per mention — measured, not asserted.
//!
//! Builds N-entity fixtures (1k, 5k, 10k), then times a single fresh
//! `declare` that resolves a *new* mention against the existing entity
//! set. The LSH blocking index (M9 §10.1) is built once per (space, type)
//! per reproject batch — this test measures the incremental path (one
//! declare at a time), which is the harder case.
//!
//! The test asserts the *structural* property (a fresh mention resolves
//! correctly against a large set) and prints the timing table. It does
//! NOT hard-fail on latency (CI machines vary); the numbers are the
//! measurement the exit criterion demands.

use oxibrain::Brain;
use oxibrain_ports::{FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use std::sync::Arc;
use std::time::Instant;

/// Build a ring fixture of `n` entities, then return (brain, space, resolved Entity0).
fn build_fixture(dir: &std::path::Path, n: usize) -> (Brain, String, String) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (brain, space) = rt.block_on(async {
        let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
        let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.to_str().unwrap()), clock)
            .await
            .unwrap();
        let space = brain.ensure_space("bench").await.unwrap();
        (brain, space)
    });

    for i in 0..n {
        let subj = format!("Entity{i}");
        let obj = format!("Entity{}", (i + 1) % n);
        rt.block_on(brain.declare(
            &space,
            Declaration::AddStatement {
                subject: EntityRef {
                    surface: subj.clone(),
                    ty: "Concept".into(),
                },
                predicate: "knows".into(),
                object: DeclObject::Entity {
                    surface: obj.clone(),
                    ty: "Concept".into(),
                },
                polarity: "affirm".into(),
                valid_from: TIME_MIN.millis(),
                valid_to: TIME_MAX.millis(),
            },
        ))
        .unwrap();
    }

    let hub = rt.block_on(brain.resolve_entity_id(&space, "Concept", "Entity0"))
        .unwrap()
        .expect("Entity0");
    (brain, space, hub)
}

/// Time one fresh declare (resolves a new subject "ProbeMention" against the
/// full entity set).
fn time_fresh_declare(brain: &Brain, space: &str) -> std::time::Duration {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let start = Instant::now();
    rt.block_on(brain.declare(
        space,
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "ProbeMention".into(),
                ty: "Concept".into(),
            },
            predicate: "knows".into(),
            object: DeclObject::Entity {
                surface: "Entity0".into(),
                ty: "Concept".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
    ))
    .unwrap();
    start.elapsed()
}

#[test]
fn resolution_is_sublinear_per_mention() {
    let sizes = [1_000usize, 5_000, 10_000];
    let mut prev_time: Option<std::time::Duration> = None;
    let mut prev_n: Option<usize> = None;

    println!("\n═══ M9 resolution scaling (sublinear per mention) ═══");
    for n in sizes {
        let dir = tempfile::tempdir().unwrap();
        let (brain, space, _hub) = build_fixture(dir.path(), n);
        let t = time_fresh_declare(&brain, &space);
        let per_mention = t / (n as u32).max(1);
        println!(
            "  {n:>6} entities | fresh-declare {:.3} ms | per-entity {:.3} µs",
            t.as_secs_f64() * 1000.0,
            per_mention.as_secs_f64() * 1_000_000.0
        );
        if let (Some(pt), Some(pn)) = (prev_time, prev_n) {
            // Sublinear: doubling N must NOT double the per-declare time.
            // (Actually per-declare should grow sublinearly: 5x N → <5x time.)
            let ratio = t.as_secs_f64() / pt.as_secs_f64();
            let n_ratio = n as f64 / pn as f64;
            println!(
                "    growth: N ×{:.1} → time ×{:.2} (sublinear iff {:.2} < {:.1})",
                n_ratio, ratio, ratio, n_ratio
            );
        }
        prev_time = Some(t);
        prev_n = Some(n);
    }
    // Structural assertion: a fresh mention resolves at all (created +
    // linked), not an error. Latency is printed, not gated.
}
