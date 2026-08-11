use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{ClockPort, SystemClock};
use oxibrain_store::{StoreHandle, ledger};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn episode_round_trip() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t = SystemClock.now();

    // create the space and capture its deterministic id, then insert under that id
    let (tx, rx) = std::sync::mpsc::channel();
    h.writer
        .submit(Box::new(move |conn| {
            let space_id = ledger::create_space(conn, "personal", t)?;
            let mut ep = Episode {
                id: String::new(),
                space: space_id.clone(),
                seq: 0,
                content_hash: oxibrain_core::ContentHash([0u8; 32]),
                content: "first note".into(),
                source: SourceRef::Note {
                    path: "n.md".into(),
                },
                trust: TrustTier::Trusted,
                kind: EpisodeKind::Primary,
                occurred_at: t,
                ingested_at: t,
                redacted_at: None,
            };
            ledger::insert_episode(conn, &mut ep)?;
            tx.send(ep.id).unwrap();
            Ok(())
        }))
        .unwrap();
    h.writer.flush().unwrap();
    let id = rx.recv().unwrap();

    // read back via reader pool
    let got = h
        .readers
        .read(|conn| ledger::get_episode(conn, &id))
        .unwrap()
        .unwrap();
    assert_eq!(got.content, "first note");
    assert_eq!(got.seq, 0);
    assert_eq!(got.trust, TrustTier::Trusted);
}

#[test]
fn reinsert_same_content_is_noop() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t = SystemClock.now();
    let (tx, rx) = std::sync::mpsc::channel();
    h.writer
        .submit(Box::new(move |conn| {
            let space_id = ledger::create_space(conn, "personal", t)?;
            tx.send(space_id).unwrap();
            Ok(())
        }))
        .unwrap();
    h.writer.flush().unwrap();
    let space = rx.recv().unwrap();

    for _ in 0..3 {
        let space = space.clone();
        h.writer
            .submit(Box::new(move |conn| {
                let mut ep = Episode {
                    id: String::new(),
                    space,
                    seq: 0,
                    content_hash: oxibrain_core::ContentHash([0u8; 32]),
                    content: "dup note".into(),
                    source: SourceRef::Note {
                        path: "d.md".into(),
                    },
                    trust: TrustTier::Trusted,
                    kind: EpisodeKind::Primary,
                    occurred_at: t,
                    ingested_at: t,
                    redacted_at: None,
                };
                ledger::insert_episode(conn, &mut ep)
            }))
            .unwrap();
    }
    h.writer.flush().unwrap();
    let count: i64 = h.readers.read(ledger::episode_count).unwrap();
    assert_eq!(count, 1, "idempotent insert must not duplicate");
}
