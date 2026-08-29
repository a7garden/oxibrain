//! Legacy content-hash dedup must adopt the EXISTING episode's identity
//! (§9.7 layer 1). Recomputing the id from the caller's `occurred_at`
//! mints an id that no episode row backs, so child rows (assertions,
//! mentions) FK-fail — the "second identical declare crashes" bug.

use oxibrain_ports::Timestamp;
use oxibrain_store::{ledger, migration};
use rusqlite::Connection;

fn fresh() -> (Connection, String) {
    // vec0 virtual tables (v5+) need the auto-extension at open time.
    oxibrain_store::migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    // Episodes FK spaces(id): the caller passes the RESOLVED space id.
    let sid = ledger::create_space(&conn, "sp", Timestamp(0)).unwrap();
    (conn, sid)
}
fn ep_in(space_id: &str, at: i64) -> oxibrain_core::Episode {
    let content = "identical content";
    oxibrain_core::Episode {
        id: String::new(),
        space: space_id.into(),
        seq: 0,
        content_hash: oxibrain_core::content_hash(content),
        content: content.into(),
        source: oxibrain_core::SourceRef::Note {
            path: "t.md".into(),
        },
        trust: oxibrain_core::TrustTier::Trusted,
        kind: oxibrain_core::EpisodeKind::Primary,
        occurred_at: Timestamp(at),
        ingested_at: Timestamp(at),
        redacted_at: None,
    }
}

#[test]
fn dedup_hit_adopts_existing_identity() {
    let (conn, sid) = fresh();
    let mut first = ep_in(&sid, 1_000);
    ledger::insert_episode(&conn, &mut first).unwrap();

    // Same content, LATER transaction time: the raw dedup path.
    let mut second = ep_in(&sid, 9_000);
    ledger::insert_episode(&conn, &mut second).unwrap();

    assert_eq!(
        first.id, second.id,
        "same content must resolve to the same episode identity"
    );
    let backed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE id = ?1",
            rusqlite::params![second.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(backed, 1, "the adopted id must be backed by an episode row");
}

#[test]
fn first_insert_is_not_a_dedup_hit() {
    let (conn, sid) = fresh();
    let mut ep = ep_in(&sid, 1_000);
    ledger::insert_episode(&conn, &mut ep).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(ep.seq, 0, "first insert takes sequence 0");
}
