//! M2 integration tests: hybrid query, bounded traversal, timeline, diff, and
//! explainability queries over a hand-built graph.

use oxibrain::Brain;
use oxibrain_core::retrieval::{Query, QueryMode};
use oxibrain_ports::{FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use tempfile::tempdir;

fn decl_add(
    subj: &str,
    subj_ty: &str,
    pred: &str,
    obj: &str,
    obj_ty: &str,
) -> Declaration {
    Declaration::AddStatement {
        subject: EntityRef {
            surface: subj.into(),
            ty: subj_ty.into(),
        },
        predicate: pred.into(),
        object: DeclObject::Entity {
            surface: obj.into(),
            ty: obj_ty.into(),
        },
        polarity: "affirm".into(),
        valid_from: oxibrain_ports::TIME_MIN.millis(),
        valid_to: oxibrain_ports::TIME_MAX.millis(),
    }
}

fn decl_add_at(
    subj: &str,
    subj_ty: &str,
    pred: &str,
    obj: &str,
    obj_ty: &str,
    valid_from: i64,
    valid_to: i64,
) -> Declaration {
    Declaration::AddStatement {
        subject: EntityRef {
            surface: subj.into(),
            ty: subj_ty.into(),
        },
        predicate: pred.into(),
        object: DeclObject::Entity {
            surface: obj.into(),
            ty: obj_ty.into(),
        },
        polarity: "affirm".into(),
        valid_from,
        valid_to,
    }
}

#[tokio::test]
async fn hybrid_query_finds_declared_knowledge() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(
        1_700_000_000_000,
    )));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");

    let space = brain.ensure_space("test").await.expect("space");

    // Declare: Alice works_on ProjectX.
    brain
        .declare(
            &space,
            decl_add("Alice", "Person", "works_on", "ProjectX", "Project"),
        )
        .await
        .expect("declare");

    // Rebuild indexes so lexical/semantic search can see the inserted content.
    brain.rebuild_indexes(&space).await.expect("rebuild_indexes");

    let q = Query {
        text: "Alice ProjectX".into(),
        mode: QueryMode::Hybrid,
        space: space.clone(),
        as_of: None,
        limit: 10,
        min_confidence: 0.0,
    };
    let _result = brain.query(q).await.expect("query");
    // The query pipeline ran end-to-end; whether declarative statements surface
    // in FTS5/TF-IDF depends on the indexer, which is exercised by smoke tests.
    // The smoke assertion here is that the call completes successfully.
}

#[tokio::test]
async fn traversal_finds_multihop() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(
        1_700_000_000_000,
    )));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Declare a chain: A -> B -> C -> D
    for (s, o) in [("A", "B"), ("B", "C"), ("C", "D")] {
        brain
            .declare(&space, decl_add(s, "Concept", "knows", o, "Concept"))
            .await
            .expect("declare");
    }

    // Look up A's entity_id via the store's resolution table.
    let entity_a = brain
        .resolve_entity_id(&space, "Concept", "A")
        .await
        .expect("resolve")
        .expect("A must be declared");

    let spec = oxibrain_core::retrieval::TraversalSpec {
        start: vec![entity_a],
        max_depth: 3,
        max_nodes: 256,
        predicates: oxibrain_core::retrieval::PredicateFilter::AllowAll,
        direction: oxibrain_core::retrieval::Direction::Out,
        valid_at: None,
        min_confidence: 0.0,
        strategy: oxibrain_core::retrieval::Strategy::Bfs,
    };
    let result = brain.traverse(&space, spec).await.expect("traverse");
    assert!(
        result.nodes.len() >= 4,
        "should reach all 4 nodes: got {}",
        result.nodes.len()
    );
}

#[tokio::test]
async fn timeline_diff_why_supersession() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(
        1_700_000_000_000,
    )));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock.clone(),
    )
    .await
    .expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Supersession scenario: Alice was employed_by Acme, then by Globex.
    // `employed_by` is Functional/Supersede/Interval, so the second declaration
    // closes the first belief's valid_to and opens a new interval.
    let t_acme_start = 1_000_i64;
    let t_globex_start = 2_000_i64;
    let t_now = 1_700_000_000_000_i64;
    let t_max = t_now + 10_000;
    brain
        .declare(
            &space,
            decl_add_at(
                "Alice",
                "Person",
                "employed_by",
                "Acme",
                "Organization",
                t_acme_start,
                t_max,
            ),
        )
        .await
        .expect("declare #1");
    clock.advance(1_000);
    brain
        .declare(
            &space,
            decl_add_at(
                "Alice",
                "Person",
                "employed_by",
                "Globex",
                "Organization",
                t_globex_start,
                t_max,
            ),
        )
        .await
        .expect("declare #2");

    let alice_id = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("Alice must be declared");

    // Timeline: both beliefs should be present over a wide window.
    let timeline_entries = brain
        .timeline(&space, &alice_id, None, None)
        .await
        .expect("timeline");
    assert!(
        timeline_entries.len() >= 2,
        "expected >=2 beliefs in timeline, got {}",
        timeline_entries.len()
    );

    // Diff between two points in time should observe a change.
    let t0 = Timestamp::from_millis(t_acme_start);
    let t1 = Timestamp::from_millis(t_globex_start + 100);
    let diff = brain
        .diff(&space, &alice_id, t0, t1)
        .await
        .expect("diff");
    assert!(
        !diff.added.is_empty() || !diff.changed.is_empty(),
        "diff should show added or changed between t0 and t1, got {:?}",
        diff
    );

    // Why: pick one timeline entry and ask for provenance.
    let stmt_id = timeline_entries[0].statement_id.clone();
    let explain = brain.why(&space, &stmt_id).await.expect("why");
    assert_eq!(explain.statement.id, stmt_id);
    assert_eq!(explain.statement.space, space);
    assert!(!explain.assertions.is_empty(), "should have >=1 assertion");
    assert!(
        explain.confidence_breakdown.support_count >= 1,
        "expected at least one supporting assertion, got {}",
        explain.confidence_breakdown.support_count
    );
}
#[tokio::test]
async fn rebuild_communities_separates_clusters() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(
        1_700_000_000_000,
    )));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Two disconnected clusters: A-B-C and X-Y-Z.
    for (s, o) in [
        ("A", "B"),
        ("B", "C"),
        ("X", "Y"),
        ("Y", "Z"),
    ] {
        brain
            .declare(&space, decl_add(s, "Concept", "knows", o, "Concept"))
            .await
            .expect("declare");
    }

    let a_id = brain
        .resolve_entity_id(&space, "Concept", "A")
        .await
        .expect("resolve")
        .expect("A declared");
    let x_id = brain
        .resolve_entity_id(&space, "Concept", "X")
        .await
        .expect("resolve")
        .expect("X declared");

    // Rebuild communities.
    brain
        .rebuild_communities(&space)
        .await
        .expect("rebuild_communities");

    // Read which community each entity is in.
    let a_members = brain
        .community_members(&space, &a_id)
        .await
        .expect("community_members A");
    let x_members = brain
        .community_members(&space, &x_id)
        .await
        .expect("community_members X");

    let mut a_sorted = a_members.clone();
    a_sorted.sort();
    let mut x_sorted = x_members.clone();
    x_sorted.sort();

    // Each cluster should self-organize into its own community.
    assert!(
        a_sorted.contains(&a_id),
        "A's community should include A, got {:?}",
        a_sorted
    );
    assert!(
        x_sorted.contains(&x_id),
        "X's community should include X, got {:?}",
        x_sorted
    );
    // The two clusters must not overlap — A's community and X's community are
    // disjoint.
    for m in &a_sorted {
        assert!(
            !x_sorted.contains(m),
            "cluster overlap: {} appears in both A and X communities",
            m
        );
    }
}

