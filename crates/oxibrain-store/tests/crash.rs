//! DESIGN.md §14.3: kill mid-ingest at each stage boundary; assert resumption
//! with no duplicate assertions. M0 stage boundary: episode insert. A duplicate
//! insert is a content-hash no-op, so recovery yields exactly one episode.

use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{ClockPort, SystemClock};
use oxibrain_store::{StoreHandle, ledger};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn reopen_after_drop_recovers_no_duplicates() {
    let dir = tempdir().unwrap();
    let t = SystemClock.now();

    // first session: insert one episode, then drop (simulated crash)
    {
        let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        h.writer
            .as_ref()
            .unwrap()
            .submit(Box::new(move |conn| {
                let space_id = ledger::create_space(conn, "personal", t)?;
                let mut ep = Episode {
                    id: String::new(),
                    space: space_id,
                    seq: 0,
                    content_hash: oxibrain_core::ContentHash([0u8; 32]),
                    content: "crash test note".into(),
                    source: SourceRef::Note {
                        path: "c.md".into(),
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
        let _first_id = rx.recv().unwrap();
        let count: i64 = h.readers.read(ledger::episode_count).unwrap();
        assert_eq!(count, 1, "first session must persist the episode");
        // "crash": drop without graceful close
        drop(h);
    }

    // second session: reopen, re-insert same content, assert exactly one episode
    {
        let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        h.writer
            .as_ref()
            .unwrap()
            .submit(Box::new(move |conn| {
                let space_id = ledger::create_space(conn, "personal", t)?;
                let mut ep = Episode {
                    id: String::new(),
                    space: space_id,
                    seq: 0,
                    content_hash: oxibrain_core::ContentHash([0u8; 32]),
                    content: "crash test note".into(),
                    source: SourceRef::Note {
                        path: "c.md".into(),
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
        let _second_id = rx.recv().unwrap();
        let count: i64 = h.readers.read(ledger::episode_count).unwrap();
        assert_eq!(
            count, 1,
            "reinsert after reopen must be idempotent (no dup)"
        );
    }
}
