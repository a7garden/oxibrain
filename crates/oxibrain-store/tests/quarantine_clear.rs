//! A successful extraction consumes the episode's failure rows: quarantine
//! is a retry queue, not an archive (storage-footprint plan, Task 1).

use oxibrain_core::{SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::{extraction, ledger, migration, quarantine};
use rusqlite::Connection;

fn setup() -> (Connection, String) {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    let space_id = ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    (conn, space_id)
}

fn episode(conn: &Connection, space_id: &str, content: &str) -> String {
    extraction::ingest_event(
        conn,
        space_id,
        content,
        SourceRef::Note {
            path: "notes/t.md".into(),
        },
        TrustTier::Trusted,
        None,
        Timestamp(2000),
    )
    .unwrap()
}

fn failure_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM extraction_failures", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn success_clears_prior_failures() {
    let (conn, space_id) = setup();
    let ep = episode(&conn, &space_id, "hello world");

    quarantine::record_failure(
        &conn,
        &ep,
        "ext1",
        "garbage",
        r#"["bad json"]"#,
        Timestamp(3000),
    )
    .unwrap();
    assert_eq!(failure_count(&conn), 1);

    // The successful cache write must consume the failure rows.
    extraction::cache_response(&conn, &ep, "ext1", r#"{"claims":[]}"#, Timestamp(4000)).unwrap();

    assert_eq!(
        failure_count(&conn),
        0,
        "successful extraction must clear its failure rows"
    );
}

#[test]
fn success_keeps_failures_of_other_extractors() {
    let (conn, space_id) = setup();
    let ep = episode(&conn, &space_id, "hello world");

    quarantine::record_failure(&conn, &ep, "ext1", "garbage", "[]", Timestamp(3000)).unwrap();
    extraction::cache_response(&conn, &ep, "ext2", r#"{"claims":[]}"#, Timestamp(4000)).unwrap();

    assert_eq!(
        failure_count(&conn),
        1,
        "only the succeeding extractor's rows are cleared"
    );
}

#[test]
fn success_keeps_failures_of_other_episodes() {
    let (conn, space_id) = setup();
    let ep_a = episode(&conn, &space_id, "alpha content");
    let ep_b = episode(&conn, &space_id, "beta content");

    quarantine::record_failure(&conn, &ep_b, "ext1", "garbage", "[]", Timestamp(3000)).unwrap();
    extraction::cache_response(&conn, &ep_a, "ext1", r#"{"claims":[]}"#, Timestamp(4000)).unwrap();

    assert_eq!(
        failure_count(&conn),
        1,
        "only the succeeded episode's rows are cleared"
    );
}
