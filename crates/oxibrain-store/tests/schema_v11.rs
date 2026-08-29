//! Schema v11 tests — queue-less extraction (two-plane design §11.1).
//!
//! Covers the migration from v10 → v11 and the fresh-DB contract:
//!   * `ingest_jobs` is dropped; the backlog becomes the
//!     `uncached_memory_episodes` query instead of a durable queue
//!   * the pre-drop row count is recorded in `meta` under
//!     `v11_ingest_jobs_dropped`
//!   * episodes/sources rows are untouched and FK-valid after the drop

use oxibrain_store::{migration, registry, schema::LEDGER_SCHEMA_VERSION};
use rusqlite::Connection;

/// Build a v10 database (the full v1..v10 chain) with one space, two
/// episodes, and three queued ingest_jobs rows.
fn build_v10_fixture(conn: &Connection) {
    migration::ensure_vec_extension();
    for sql in [
        include_str!("../src/migrations/v1.sql"),
        include_str!("../src/migrations/v2.sql"),
        include_str!("../src/migrations/v3.sql"),
        include_str!("../src/migrations/v4.sql"),
        include_str!("../src/migrations/v5.sql"),
        include_str!("../src/migrations/v6.sql"),
        include_str!("../src/migrations/v7.sql"),
        include_str!("../src/migrations/v8.sql"),
        include_str!("../src/migrations/v9.sql"),
        include_str!("../src/migrations/v10.sql"),
    ] {
        conn.execute_batch(sql).unwrap();
    }
    registry::seed_core_v1(conn).unwrap();
    conn.pragma_update(None, "user_version", 10i64).unwrap();
    conn.execute_batch(
        "INSERT INTO spaces (id, name, created_at) VALUES ('sp1', 'test', 1000);
         INSERT INTO episodes
           (id, space_id, seq, content_hash, content, source_kind, source_ref,
            trust, kind, occurred_at, ingested_at)
         VALUES ('ep1', 'sp1', 0, x'00', 'first content', 'note', 'a.md',
                 'trusted', 'primary', 1000, 1000),
                ('ep2', 'sp1', 1, x'01', 'second content', 'note', 'b.md',
                 'trusted', 'primary', 1001, 1001);
         INSERT INTO ingest_jobs
           (id, episode_id, extractor_id, state, attempts, created_at, updated_at)
         VALUES ('j1', 'ep1', 'ext1', 'done', 0, 1000, 1000),
                ('j2', 'ep1', 'ext2', 'ready', 1, 1000, 1000),
                ('j3', 'ep2', 'ext1', 'failed', 3, 1000, 1000);",
    )
    .expect("insert v10 fixture data");
}

/// Check whether a table exists.
fn has_table(conn: &Connection, name: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
}

/// Read a meta value.
fn meta_value(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

/// Fresh DB migrates to v11 with the queue gone; a fresh store never had
/// queued rows, so the recorded drop count is zero.
#[test]
fn fresh_db_reaches_v11_without_queue() {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    assert_eq!(LEDGER_SCHEMA_VERSION, 12);

    assert!(
        !has_table(&conn, "ingest_jobs"),
        "queue must be gone at v11"
    );
    assert_eq!(
        meta_value(&conn, "v11_ingest_jobs_dropped").as_deref(),
        Some("0"),
        "fresh store records a zero pre-drop count"
    );
}

/// A v10 fixture with queued jobs upgrades to v11: the table is dropped,
/// the pre-drop count is recorded, and episodes survive FK-valid.
#[test]
fn migrates_from_v10_with_data() {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    build_v10_fixture(&conn);

    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    assert_eq!(LEDGER_SCHEMA_VERSION, 12);

    // Queue table gone (index goes with it).
    assert!(!has_table(&conn, "ingest_jobs"));

    // Pre-drop count recorded.
    assert_eq!(
        meta_value(&conn, "v11_ingest_jobs_dropped").as_deref(),
        Some("3")
    );

    // Episodes untouched: same rows, same contents.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let first: String = conn
        .query_row("SELECT content FROM episodes WHERE id = 'ep1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(first, "first content");

    // FK integrity: no dangling references anywhere.
    let fk_violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk_violations, 0, "foreign key check must be clean");

    // Re-running the migration is a no-op.
    let again = migration::run(&conn).unwrap();
    assert_eq!(again, LEDGER_SCHEMA_VERSION);
    assert_eq!(
        meta_value(&conn, "v11_ingest_jobs_dropped").as_deref(),
        Some("3"),
        "idempotent re-run must not re-count (table already gone)"
    );
}
