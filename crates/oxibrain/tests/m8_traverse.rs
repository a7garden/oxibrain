//! M8 §8.5 — belief-filtered adjacency (F11).
//!
//! A retracted edge must not appear in a `traverse` result. Before M8 the
//! query pulled edges from `statements` alone, so retracted edges stayed
//! alive in the adjacency graph. After M8, `load_adjacency` joins beliefs
//! and excludes `Retracted` and `Contradicted` rows.

use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::retrieval::{Direction, PredicateFilter, Strategy, TraversalSpec};
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

async fn find_statement_id(brain: &Brain, space: &str, surface: &str, ty: &str) -> String {
    let entity_id = brain
        .resolve_entity_id(space, ty, surface)
        .await
        .expect("resolve")
        .expect("entity");
    let beliefs = brain.beliefs(space, &entity_id).await.expect("beliefs");
    assert!(
        !beliefs.is_empty(),
        "expected at least one belief for {surface}:{ty}"
    );
    beliefs[0].statement.clone()
}

#[tokio::test]
async fn traverse_excludes_retracted_edges() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path().to_str().unwrap()), clock)
        .await
        .expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Chain A -> B -> C
    brain
        .declare(&space, decl_add("A", "Concept", "knows", "B", "Concept"))
        .await
        .expect("declare A->B");
    brain
        .declare(&space, decl_add("B", "Concept", "knows", "C", "Concept"))
        .await
        .expect("declare B->C");

    let a = brain
        .resolve_entity_id(&space, "Concept", "A")
        .await
        .expect("resolve")
        .expect("A");
    let c = brain
        .resolve_entity_id(&space, "Concept", "C")
        .await
        .expect("resolve")
        .expect("C");

    // Pre-retraction: A reaches C in 2 hops.
    let pre_spec = TraversalSpec {
        start: vec![a.clone()],
        max_depth: 2,
        max_nodes: 64,
        predicates: PredicateFilter::AllowAll,
        direction: Direction::Out,
        valid_at: None,
        min_confidence: 0.0,
        strategy: Strategy::Bfs,
    };
    let pre = brain
        .traverse(&space, pre_spec)
        .await
        .expect("pre-traverse");
    let pre_targets: Vec<String> = pre.nodes.iter().map(|n| n.entity.clone()).collect();
    assert!(
        pre_targets.contains(&c),
        "C must be reachable before retraction; got {:?}",
        pre_targets
    );

    // Find B's statement_id and retract it.
    let stmt_bc = find_statement_id(&brain, &space, "B", "Concept").await;
    brain
        .declare(
            &space,
            Declaration::Retract {
                subject: entity("B", "Concept"),
                predicate: "knows".into(),
                object: DeclObject::Entity {
                    surface: "C".into(),
                    ty: "Concept".into(),
                },
                episode: stmt_bc,
            },
        )
        .await
        .expect("retract");

    // Post-retraction: A no longer reaches C. The B->C edge is excluded
    // by the joined query in `load_adjacency`.
    let post_spec = TraversalSpec {
        start: vec![a.clone()],
        max_depth: 2,
        max_nodes: 64,
        predicates: PredicateFilter::AllowAll,
        direction: Direction::Out,
        valid_at: None,
        min_confidence: 0.0,
        strategy: Strategy::Bfs,
    };
    let post = brain
        .traverse(&space, post_spec)
        .await
        .expect("post-traverse");
    let post_targets: Vec<String> = post.nodes.iter().map(|n| n.entity.clone()).collect();
    assert!(
        !post_targets.contains(&c),
        "C must NOT be reachable after retraction; got {:?}",
        post_targets
    );
}
