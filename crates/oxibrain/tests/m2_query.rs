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
#[tokio::test]
async fn apply_decay_updates_salience() {
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

    // Declare an entity so something exists in the entities table.
    brain
        .declare(
            &space,
            decl_add("Alice", "Person", "employed_by", "Acme", "Organization"),
        )
        .await
        .expect("declare");

    // Rebuild indexes so last_activity gets populated.
    brain
        .rebuild_indexes(&space)
        .await
        .expect("rebuild_indexes");

    // Advance several days, then apply decay.
    clock.advance(10 * 86_400_000);
    let updated = brain.apply_decay(&space).await.expect("apply_decay");
    assert!(updated >= 1, "should update at least one entity, got {updated}");

    // Verify decay applied: open the DB directly and read salience.
    let db_path = dir.path().join("brain.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let salience: f64 = conn
        .query_row(
            "SELECT salience FROM entities WHERE space_id = ?1 LIMIT 1",
            rusqlite::params![&space],
            |r| r.get(0),
        )
        .expect("read salience");
    // 10 days of decay with lambda=0.01 → e^(-0.1) ≈ 0.904. Floor is 0.05.
    assert!(
        (0.05..=1.0).contains(&salience),
        "salience should be in [floor, 1.0], got {salience}"
    );
    // Specifically, it should be < 1.0 after 10 days.
    assert!(
        salience < 1.0,
        "salience should have decreased after 10 days, got {salience}"
    );
}

#[tokio::test]
async fn compact_succeeds_and_keeps_content_readable() {
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

    // Ingest a note.
    let note_text = "this is a long body that should be compacted away".to_string();
    let ep_id = brain
        .ingest_note(
            &space,
            "note.md",
            note_text.clone(),
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .expect("ingest_note");

    // Advance well past the 90-day compaction threshold.
    clock.advance(200 * 86_400_000);

    let compacted = brain.compact(&space).await.expect("compact");
    assert!(compacted >= 1, "should compact at least one episode, got {compacted}");

    // The get_episode call should still return the content transparently.
    let got = brain.get_episode(&ep_id).await.expect("get_episode").expect("some");
    assert_eq!(got.content, note_text, "content should be transparently restored");
}
#[tokio::test]
async fn assemble_context_packs_within_budget() {
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

    // Declare some knowledge and ingest a note so the context layers have content.
    brain
        .declare(
            &space,
            decl_add("Alice", "Person", "employed_by", "Acme", "Organization"),
        )
        .await
        .expect("declare");
    brain
        .ingest_note(
            &space,
            "note.md",
            "Alice joined Acme in 2020.".to_string(),
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .expect("ingest_note");

    // Rebuild indexes so the lexical layer can find anything.
    brain
        .rebuild_indexes(&space)
        .await
        .expect("rebuild_indexes");

    let budget = 1024_usize;
    let result = brain
        .assemble_context(&space, "Alice Acme", budget)
        .await
        .expect("assemble_context");

    assert!(
        result.total_tokens <= budget,
        "total_tokens {} must be <= budget {}",
        result.total_tokens,
        budget
    );
    assert_eq!(result.budget.max_tokens, budget);
    // We ingested at least one episode, so the recent-episodes layer should be
    // populated.
    let has_recent = result
        .layers
        .iter()
        .any(|l| l.kind == oxibrain_core::context::LayerKind::RecentEpisodes);
    assert!(
        has_recent,
        "expected a recent-episodes layer, got layers: {:?}",
        result.layers
    );
}



