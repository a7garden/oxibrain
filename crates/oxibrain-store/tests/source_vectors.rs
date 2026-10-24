use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::{ledger, migration, read_evidence::SourceReadScope, source_vectors};
use rusqlite::Connection;
fn setup() -> Connection {
    migration::ensure_vec_extension();
    let c = Connection::open_in_memory().unwrap();
    migration::run(&c).unwrap();
    c.execute(
        "INSERT INTO spaces(id,name,created_at)VALUES('s','fixture',0)",
        [],
    )
    .unwrap();
    let mut episode = Episode {
        id: String::new(),
        space: "s".into(),
        seq: 0,
        content_hash: ContentHash([0; 32]),
        content: "Original source bytes".into(),
        source: SourceRef::Session {
            session_id: "session".into(),
        },
        trust: TrustTier::Untrusted,
        kind: EpisodeKind::Primary,
        occurred_at: Timestamp(1),
        ingested_at: Timestamp(2),
        redacted_at: None,
    };
    ledger::insert_episode(&c, &mut episode).unwrap();
    c
}
#[test]
fn source_vector_revision_model_policy_and_redaction_are_enforced() {
    let c = setup();
    let scope = SourceReadScope {
        space: "s",
        known_at: Timestamp(10),
        as_of: Timestamp(10),
        excluded_session: None,
    };
    let pending = source_vectors::pending(&c, "s", "model-a", 10).unwrap();
    assert_eq!(pending.len(), 1);
    let rows = vec![(pending[0].clone(), vec![1.0, 0.0])];
    assert_eq!(source_vectors::upsert(&c, "model-a", &rows).unwrap(), 1);
    assert_eq!(
        source_vectors::coverage(&c, &scope, "model-a").unwrap(),
        (1, 1)
    );
    assert_eq!(
        source_vectors::coverage(&c, &scope, "model-b").unwrap(),
        (1, 0)
    );
    c.execute(
        "UPDATE memory_source_vectors SET window_policy='different-policy'",
        [],
    )
    .unwrap();
    assert_eq!(
        source_vectors::coverage(&c, &scope, "model-a").unwrap(),
        (1, 0)
    );
    source_vectors::upsert(&c, "model-a", &rows).unwrap();
    let mut stale = pending[0].clone();
    stale.revision = "invalid".into();
    assert_eq!(
        source_vectors::upsert(&c, "model-a", &[(stale, vec![0.0, 1.0])]).unwrap(),
        0
    );
    let batch = source_vectors::batch(&c, &scope, "model-a", "", 10).unwrap();
    assert_eq!(batch[0].vector, vec![1.0, 0.0]);
    let excluded = SourceReadScope {
        excluded_session: Some("session"),
        ..scope
    };
    assert_eq!(
        source_vectors::coverage(&c, &excluded, "model-a").unwrap(),
        (0, 0)
    );
    c.execute("UPDATE episodes SET redacted_at=20,content=''", [])
        .unwrap();
    assert_eq!(
        c.query_row::<usize, _, _>("SELECT count(*) FROM memory_source_vectors", [], |r| r
            .get(0))
            .unwrap(),
        0
    );
    assert_eq!(source_vectors::upsert(&c, "model-a", &rows).unwrap(), 0);
}
#[test]
fn source_vector_projection_is_discarded_and_reconstructible() {
    let c = setup();
    let pending = source_vectors::pending(&c, "s", "m", 10).unwrap();
    source_vectors::upsert(&c, "m", &[(pending[0].clone(), vec![1.0])]).unwrap();
    oxibrain_store::read_evidence::rebuild_raw(&c, "s").unwrap();
    assert_eq!(
        c.query_row::<usize, _, _>("SELECT count(*) FROM memory_source_vectors", [], |r| r
            .get(0))
            .unwrap(),
        0
    );
    assert_eq!(
        source_vectors::pending(&c, "s", "m", 10).unwrap()[0].text,
        "Original source bytes"
    );
    assert!(source_vectors::upsert(&c, "m", &[(pending[0].clone(), vec![f32::NAN])]).is_err());
}
