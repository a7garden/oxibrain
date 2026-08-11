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
    brain.rebuild_indexes(&space).await.expect("rebuild_indexes");
    brain
        .rebuild_communities(&space)
        .await
        .expect("rebuild_communities");

    // Snapshot index tables after incremental projection.
    let snapshot1 = brain.snapshot_indexes(&space).await.expect("snapshot1");
    assert!(
        !snapshot1.is_empty(),
        "index snapshot must be non-empty after declares — projection or rebuild is broken"
    );
    assert!(
        snapshot1.contains("---fts---"),
        "snapshot missing fts section: {snapshot1}"
    );
    assert!(
        snapshot1.contains("---vec---"),
        "snapshot missing vec section: {snapshot1}"
    );
    assert!(
        snapshot1.contains("---com---"),
        "snapshot missing com section: {snapshot1}"
    );

    // Reproject — must rebuild the same index state byte-for-byte.
    brain.reproject().await.expect("reproject");

    // Confirm the projection is still consistent (Alice still resolves).
    let alice_id_after = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("Alice exists after reproject");
    assert_eq!(alice_id, alice_id_after, "entity ids must survive reproject");

    let snapshot2 = brain.snapshot_indexes(&space).await.expect("snapshot2");
    assert_eq!(
        snapshot1, snapshot2,
        "index tables must be byte-identical after reproject"
    );
}