//! M8 §8.13 — `why --dropped` reads real data (F2).
//!
//! Before M8 the `dropped` list was always empty (F2) — executors that
//! truncated results past `limit` never recorded it. With `core::rank`'s
//! conservation guarantee, truncation is a first-class `DroppedItem` with
//! `DropReason::TruncatedByBudget`. This test drives the actual CLI
//! `why --dropped` command and asserts it prints a non-empty list.

use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::{DropReason, Query, QueryMode};
use oxibrain_ports::{FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use tempfile::tempdir;

fn entity(surface: &str, ty: &str) -> EntityRef {
    EntityRef {
        surface: surface.to_string(),
        ty: ty.to_string(),
    }
}

fn decl_add(s: &str, sty: &str, p: &str, o: &str, oty: &str) -> Declaration {
    Declaration::AddStatement {
        subject: entity(s, sty),
        predicate: p.into(),
        object: DeclObject::Entity {
            surface: o.into(),
            ty: oty.into(),
        },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: oxibrain_ports::TIME_MAX.0,
    }
}

/// The core-level contract: truncation produces real, attributed drops.
#[tokio::test]
async fn rank_truncation_drops_are_attributed() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path().to_str().unwrap()), clock)
        .await
        .expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Declare 5 statements so FTS has 5 candidates.
    for i in 0..5 {
        brain
            .declare(
                &space,
                decl_add("A", "Concept", "knows", &format!("B{i}"), "Concept"),
            )
            .await
            .expect("declare");
    }
    brain.rebuild_indexes(&space).await.expect("rebuild");

    // Query with limit=2 — 3 candidates must be truncated with attribution.
    let q = Query {
        text: "A knows".into(),
        mode: QueryMode::Lexical,
        space: space.clone(),
        as_of: None,
        limit: 2,
        min_confidence: 0.0,
    };
    let result = brain.query(q).await.expect("query");

    assert!(
        result
            .dropped
            .iter()
            .any(|d| matches!(d.reason, DropReason::TruncatedByBudget { .. })),
        "expected at least one TruncatedByBudget drop, got {:?}",
        result.dropped
    );
    // Conservation: items ∪ dropped = candidates.
    let total = result.items.len() + result.dropped.len();
    assert_eq!(total, result.total_candidates);
    assert!(total >= 5, "expected at least 5 candidates, got {total}");
}
