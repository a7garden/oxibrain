//! M8 exit criteria — filters are honored end-to-end.
//!
//! - `search(as_of = T)` returns a different result set than `search()` on a
//!   fixture where beliefs changed (this failed in three executors pre-M8).
//! - `traverse(depth=2, min_confidence=0.8, valid_at=t)` excludes retracted
//!   edges.
//!
//! The `as_of` path exercises the full P9 chain: store batch-fetches facts,
//! `core::rank` applies the fold-dependent filter.

use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::retrieval::{Direction, PredicateFilter, Query, QueryMode, Strategy, TraversalSpec};
use oxibrain_ports::{FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use tempfile::tempdir;

fn entity(surface: &str, ty: &str) -> EntityRef {
    EntityRef { surface: surface.to_string(), ty: ty.to_string() }
}

fn decl_add(s: &str, sty: &str, p: &str, o: &str, oty: &str) -> Declaration {
    Declaration::AddStatement {
        subject: entity(s, sty),
        predicate: p.into(),
        object: DeclObject::Entity { surface: o.into(), ty: oty.into() },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: oxibrain_ports::TIME_MAX.0,
    }
}

/// Fixture: Alice employed_by Acme, then employed_by Globex (Functional/
/// Supersede/Interval). At t=2000 the second declaration closes the first
/// interval. Querying as_of=1500 vs as_of=2500 must surface different
/// belief sets.
#[tokio::test]
async fn search_as_of_returns_different_result_set() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path().to_str().unwrap()), clock).await.expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    brain
        .declare(
            &space,
            Declaration::AddStatement {
                subject: entity("Alice", "Person"),
                predicate: "employed_by".into(),
                object: DeclObject::Entity { surface: "Acme".into(), ty: "Organization".into() },
                polarity: "affirm".into(),
                valid_from: 1_000,
                valid_to: 2_000,
            },
        )
        .await
        .expect("declare #1");
    brain
        .declare(
            &space,
            Declaration::AddStatement {
                subject: entity("Alice", "Person"),
                predicate: "employed_by".into(),
                object: DeclObject::Entity { surface: "Globex".into(), ty: "Organization".into() },
                polarity: "affirm".into(),
                valid_from: 2_000,
                valid_to: oxibrain_ports::TIME_MAX.0,
            },
        )
        .await
        .expect("declare #2");

    // Index the statements so FTS5 surfaces them.
    brain.rebuild_indexes(&space).await.expect("rebuild indexes");

    // Search "Alice employed" without as_of — both beliefs are live (the
    // current fold shows Globex as the active employer and Acme as
    // superseded). The ranking half indexes statements, so both surfaces
    // appear.
    let q_no_asof = Query {
        text: "Alice employed".into(),
        mode: QueryMode::Hybrid,
        space: space.clone(),
        as_of: None,
        limit: 20,
        min_confidence: 0.0,
    };
    let no_asof = brain.query(q_no_asof).await.expect("query no-as-of");

    // as_of=1500 — only the Acme belief was valid then. Globex hadn't
    // started yet, so the Globex statement's belief is outside its
    // valid window and must be dropped by `rank`.
    let q_asof = Query {
        text: "Alice employed".into(),
        mode: QueryMode::Hybrid,
        space: space.clone(),
        as_of: Some(Timestamp(1_500)),
        limit: 20,
        min_confidence: 0.0,
    };
    let asof = brain.query(q_asof).await.expect("query as-of");

    let keys_no_asof: std::collections::HashSet<String> = no_asof
        .items
        .iter()
        .map(|i| i.target.rrf_key())
        .collect();
    let keys_asof: std::collections::HashSet<String> = asof
        .items
        .iter()
        .map(|i| i.target.rrf_key())
        .collect();

    assert_ne!(
        keys_no_asof, keys_asof,
        "as_of filter must change the result set; no-as-of {:?} vs as-of {:?}",
        keys_no_asof, keys_asof
    );
}

/// traverse honors valid_at + min_confidence — retracted edges are excluded.
/// Uses the same fixture as §8.5 but with explicit valid_at.
#[tokio::test]
async fn traverse_valid_at_excludes_retracted_edge() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path().to_str().unwrap()), clock).await.expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Chain A -> B -> C, then retract B->C.
    brain.declare(&space, decl_add("A", "Concept", "knows", "B", "Concept")).await.expect("A->B");
    brain.declare(&space, decl_add("B", "Concept", "knows", "C", "Concept")).await.expect("B->C");

    let a = brain.resolve_entity_id(&space, "Concept", "A").await.expect("resolve").expect("A");
    let c = brain.resolve_entity_id(&space, "Concept", "C").await.expect("resolve").expect("C");

    // Pre-retraction with valid_at = now: C reachable.
    let pre = TraversalSpec {
        start: vec![a.clone()],
        max_depth: 2,
        max_nodes: 64,
        predicates: PredicateFilter::AllowAll,
        direction: Direction::Out,
        valid_at: Some(Timestamp(1_700_000_000_000)),
        min_confidence: 0.8,
        strategy: Strategy::Bfs,
    };
    let pre_res = brain.traverse(&space, pre).await.expect("pre");
    assert!(
        pre_res.nodes.iter().any(|n| n.entity == c),
        "C reachable pre-retraction"
    );

    // Retract B->C.
    let b_entity = brain.resolve_entity_id(&space, "Concept", "B").await.expect("resolve").expect("B");
    let beliefs = brain.beliefs(&space, &b_entity).await.expect("beliefs");
    let stmt_bc = beliefs[0].statement.clone();
    brain
        .declare(
            &space,
            Declaration::Retract {
                subject: entity("B", "Concept"),
                predicate: "knows".into(),
                object: DeclObject::Entity { surface: "C".into(), ty: "Concept".into() },
                episode: stmt_bc,
            },
        )
        .await
        .expect("retract");

    // Post-retraction with valid_at = now, min_confidence 0.8: C gone.
    let post = TraversalSpec {
        start: vec![a.clone()],
        max_depth: 2,
        max_nodes: 64,
        predicates: PredicateFilter::AllowAll,
        direction: Direction::Out,
        valid_at: Some(Timestamp(1_700_000_000_000)),
        min_confidence: 0.8,
        strategy: Strategy::Bfs,
    };
    let post_res = brain.traverse(&space, post).await.expect("post");
    assert!(
        !post_res.nodes.iter().any(|n| n.entity == c),
        "C must not be reachable after retraction with valid_at filter"
    );
}
