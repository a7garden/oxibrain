use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::{StoreHandle, ledger};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn list_spaces_orders_by_creation_and_counts() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t_personal = Timestamp::from_millis(100);
    let t_work = Timestamp::from_millis(200);
    let t_alpha = Timestamp::from_millis(100);

    let (tx, rx) = std::sync::mpsc::channel();
    h.writer
        .as_ref()
        .unwrap()
        .submit(Box::new(move |conn| {
            ledger::create_space(conn, "personal", t_personal)?;
            ledger::create_space(conn, "work", t_work)?;
            ledger::create_space(conn, "alpha", t_alpha)?;
            tx.send(()).unwrap();
            Ok(())
        }))
        .unwrap();
    h.writer.as_ref().unwrap().flush().unwrap();
    rx.recv().unwrap();

    let rows = h.readers.read(ledger::list_spaces).unwrap();
    assert_eq!(rows.len(), 3);
    // Order: (created_at, id) — alpha vs personal tie broken by id (blake3 of name).
    assert_eq!(rows[0].name, "alpha");
    assert_eq!(rows[1].name, "personal");
    assert_eq!(rows[2].name, "work");
    let work = rows.iter().find(|r| r.name == "work").unwrap();
    assert_eq!(work.episode_count, 0);
    assert_eq!(work.entity_count, 0);
}

#[test]
fn list_spaces_counts_ingested_episodes() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t = Timestamp::from_millis(1_000);

    let (tx, rx) = std::sync::mpsc::channel();
    h.writer
        .as_ref()
        .unwrap()
        .submit(Box::new(move |conn| {
            let space_id = ledger::create_space(conn, "notes", t)?;
            let mut ep = Episode {
                id: String::new(),
                space: space_id.clone(),
                seq: 0,
                content_hash: oxibrain_core::ContentHash([0u8; 32]),
                content: "hello".into(),
                source: SourceRef::Note {
                    path: "a.md".into(),
                },
                trust: TrustTier::Trusted,
                kind: EpisodeKind::Primary,
                occurred_at: t,
                ingested_at: t,
                redacted_at: None,
            };
            ledger::insert_episode(conn, &mut ep)?;
            tx.send(()).unwrap();
            Ok(())
        }))
        .unwrap();
    h.writer.as_ref().unwrap().flush().unwrap();
    rx.recv().unwrap();

    let rows = h.readers.read(ledger::list_spaces).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "notes");
    assert!(
        rows[0].episode_count >= 1,
        "expected >=1 episode, got {}",
        rows[0].episode_count
    );
    assert_eq!(rows[0].entity_count, 0);
}
