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
        .as_ref()
        .unwrap()
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
    h.writer.as_ref().unwrap().flush().unwrap();
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
        .as_ref()
        .unwrap()
        .submit(Box::new(move |conn| {
            let space_id = ledger::create_space(conn, "personal", t)?;
            tx.send(space_id).unwrap();
            Ok(())
        }))
        .unwrap();
    h.writer.as_ref().unwrap().flush().unwrap();
    let space = rx.recv().unwrap();

    for _ in 0..3 {
        let space = space.clone();
        h.writer
            .as_ref()
            .unwrap()
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
    h.writer.as_ref().unwrap().flush().unwrap();
    let count: i64 = h.readers.read(ledger::episode_count).unwrap();
    assert_eq!(count, 1, "idempotent insert must not duplicate");
}

#[test]
fn note_hashes_by_path_groups_live_notes_only() {
    use oxibrain_core::content_hash;
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t = SystemClock.now();
    let (tx, rx) = std::sync::mpsc::channel();
    let space = {
        h.writer
            .as_ref()
            .unwrap()
            .submit(Box::new(move |conn| {
                let space_id = ledger::create_space(conn, "personal", t)?;
                let note = |path: &str, content: &str, redacted: bool| Episode {
                    id: String::new(),
                    space: space_id.clone(),
                    seq: 0,
                    content_hash: oxibrain_core::ContentHash([0u8; 32]),
                    content: content.into(),
                    source: SourceRef::Note { path: path.into() },
                    trust: TrustTier::Trusted,
                    kind: EpisodeKind::Primary,
                    occurred_at: t,
                    ingested_at: t,
                    redacted_at: redacted.then_some(t),
                };
                let mut a1 = note("a.md", "v1", false);
                let mut a2 = note("a.md", "v2", false);
                let mut b = note("b.md", "b content", false);
                let mut c = note("c.md", "redacted", true);
                let mut conv = Episode {
                    source: SourceRef::Conversation,
                    content: "no path".into(),
                    ..note("ignored.md", "ignored", false)
                };
                ledger::insert_episode(conn, &mut a1)?;
                ledger::insert_episode(conn, &mut a2)?;
                ledger::insert_episode(conn, &mut b)?;
                ledger::insert_episode(conn, &mut c)?;
                ledger::insert_episode(conn, &mut conv)?;
                tx.send(space_id).unwrap();
                Ok(())
            }))
            .unwrap();
        h.writer.as_ref().unwrap().flush().unwrap();
        rx.recv().unwrap()
    };

    let map = h
        .readers
        .read(|conn| ledger::note_hashes_by_path(conn, &space))
        .unwrap();

    assert_eq!(
        map.len(),
        2,
        "conversation episode and redacted note must not appear: {map:?}"
    );
    let expected_a: HashSet<_> = [content_hash("v1"), content_hash("v2")].into();
    assert_eq!(map.get("a.md"), Some(&expected_a));
    let expected_b: HashSet<_> = [content_hash("b content")].into();
    assert_eq!(map.get("b.md"), Some(&expected_b));
}
