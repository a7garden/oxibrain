//! Index rebuild determinism: reproject produces byte-identical index tables
//! (FTS5, TF-IDF, communities) for every space.

use oxibrain::Brain;
use oxibrain_ports::*;
use oxibrain_store::project::*;
use tempfile::tempdir;

#[tokio::test]
async fn index_rebuild_is_deterministic() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");

    let space = brain.ensure_space("test").await.expect("space");

    // Declare a handful of statements across entities and predicates.
    let decls = vec![
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Alice".into(),
                ty: "Person".into(),
            },
            predicate: "works_on".into(),
            object: DeclObject::Entity {
                surface: "ProjectX".into(),
                ty: "Project".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Bob".into(),
                ty: "Person".into(),
            },
            predicate: "knows".into(),
            object: DeclObject::Entity {
                surface: "Alice".into(),
                ty: "Person".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Carol".into(),
                ty: "Person".into(),
            },
            predicate: "works_on".into(),
            object: DeclObject::Entity {
                surface: "ProjectX".into(),
                ty: "Project".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
    ];
    for decl in &decls {
        brain.declare(&space, decl.clone()).await.expect("declare");
    }

    // Sanity-check that entity resolution works (keeps the imports honest).
    let alice_id = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("Alice exists");

    // Declarations do not auto-update derived indexes — explicitly rebuild so
    // the first snapshot reflects the full index state. reproject must
    // produce the same state.
    brain
        .rebuild_indexes(&space)
        .await
        .expect("rebuild_indexes");
    brain
        .rebuild_communities(&space)
        .await
        .expect("rebuild_communities");

    // Snapshot the truth half after incremental projection (P1: byte-identical).
    let truth1 = brain.snapshot_truth(&space).await.expect("truth1");
    assert!(
        !truth1.is_empty(),
        "truth snapshot must be non-empty after declares"
    );
    assert!(
        truth1.contains("---entities---"),
        "truth snapshot missing entities section"
    );
    assert!(
        truth1.contains("---statements---"),
        "truth snapshot missing statements section"
    );
    assert!(
        truth1.contains("---beliefs---"),
        "truth snapshot missing beliefs section"
    );

    // Snapshot the ranking half (equivalent contract, currently deterministic).
    let ranking1 = brain.snapshot_ranking(&space).await.expect("ranking1");
    assert!(
        ranking1.contains("---fts_word---"),
        "ranking snapshot missing fts_word section"
    );
    assert!(
        ranking1.contains("---vectors---"),
        "ranking snapshot missing vectors section"
    );

    // Reproject — must rebuild the same state byte-for-byte.
    brain.reproject().await.expect("reproject");

    // Confirm the projection is still consistent (Alice still resolves).
    let alice_id_after = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("Alice exists after reproject");
    assert_eq!(
        alice_id, alice_id_after,
        "entity ids must survive reproject"
    );

    let truth2 = brain.snapshot_truth(&space).await.expect("truth2");
    assert_eq!(
        truth1, truth2,
        "truth half must be byte-identical after reproject"
    );
    let ranking2 = brain.snapshot_ranking(&space).await.expect("ranking2");
    assert_eq!(
        ranking1, ranking2,
        "ranking half must be equivalent after reproject"
    );
}
