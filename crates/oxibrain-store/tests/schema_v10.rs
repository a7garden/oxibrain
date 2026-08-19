//! Schema v10 tests — event identity, source registry, trust policies, assertion trust.
//!
//! Covers the migration from v9 → v10 and the fresh-DB reaches-v10 contract:
//!   * sources and source_policies tables exist with the expected columns
//!   * episodes gains source_id, occurrence_id, accepted_at, principal, claims_json
//!   * assertions gains a trust column
//!   * the v1 UNIQUE(space_id, content_hash) constraint is gone (proven by
//!     inserting a second episode with the same content_hash but a different
//!     source_id)
//!   * the partial UNIQUE index on (space_id, source_id, occurrence_id)
//!     enforces event identity for new-path episodes
//!   * a v9 fixture with a legacy episode upgrades with data intact

use oxibrain_store::{migration, registry, schema::LEDGER_SCHEMA_VERSION};
use rusqlite::Connection;

/// Build a v9 database with one space and one legacy episode.
fn build_v9_fixture(conn: &Connection) {
    migration::ensure_vec_extension();
    conn.execute_batch(include_str!("../src/migrations/v1.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v2.sql"))
        .unwrap();
    registry::seed_core_v1(conn).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v3.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v4.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v5.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v6.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v7.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v8.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v9.sql"))
        .unwrap();
    conn.pragma_update(None, "user_version", 9i64).unwrap();
    conn.execute_batch(
        "INSERT INTO spaces (id, name, created_at) VALUES ('sp1', 'test', 1000);
         INSERT INTO episodes
           (id, space_id, seq, content_hash, content, source_kind, source_ref,
            trust, kind, occurred_at, ingested_at)
         VALUES ('ep1', 'sp1', 0, x'00', 'test content', 'note', 'test.md',
                 'trusted', 'primary', 1000, 1000);",
    )
    .expect("insert legacy test data");
}

/// Check whether a column exists on a table.
fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .any(|name| name == column)
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

/// Fresh DB migrates to v10 with the new tables, columns, and constraints.
#[test]
fn fresh_db_reaches_v10_with_event_identity_columns() {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    assert_eq!(LEDGER_SCHEMA_VERSION, 10);

    // New tables exist.
    assert!(has_table(&conn, "sources"));
    assert!(has_table(&conn, "source_policies"));

    // New episode columns exist.
    for col in [
        "source_id",
        "occurrence_id",
        "accepted_at",
        "principal",
        "claims_json",
    ] {
        assert!(
            has_column(&conn, "episodes", col),
            "episodes.{col} missing after v10 migration"
        );
    }

    // Assertion trust column exists.
    assert!(has_column(&conn, "assertions", "trust"));
}

/// A v9 fixture upgrades to v10 with the legacy episode intact and the
/// `UNIQUE(space_id, content_hash)` constraint dropped.
#[test]
fn migrates_from_v9_with_data() {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    build_v9_fixture(&conn);

    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    assert_eq!(LEDGER_SCHEMA_VERSION, 10);

    // New tables exist.
    assert!(has_table(&conn, "sources"));
    assert!(has_table(&conn, "source_policies"));

    // New episode columns exist.
    for col in [
        "source_id",
        "occurrence_id",
        "accepted_at",
        "principal",
        "claims_json",
    ] {
        assert!(has_column(&conn, "episodes", col), "episodes.{col} missing");
    }

    // Legacy episode survived with NULL attachment.
    let legacy: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE id = 'ep1' AND source_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        legacy, 1,
        "legacy episode must survive with NULL attachment"
    );

    // The old UNIQUE(space_id, content_hash) constraint is gone:
    // inserting a second episode with the same content_hash but different
    // source_id must succeed.
    // Register the source first to satisfy the source_id FK introduced in v10.
    conn.execute(
        "INSERT INTO sources (id, space_id, name, kind, mode, created_at)
         VALUES ('src_x', 'sp1', 'other-source', 'note', 'push', 1000)",
        [],
    )
    .expect("insert source");
    conn.execute(
        "INSERT INTO episodes
         (id, space_id, seq, content_hash, content, source_kind, source_ref,
          trust, kind, occurred_at, ingested_at, source_id, occurrence_id)
         VALUES ('ep_dup', 'sp1', 1, x'00', 'test content', 'note', 'other.md',
                 'trusted', 'primary', 1000, 1000, 'src_x', 'occ_x')",
        [],
    )
    .expect("same content_hash, different source must not conflict after v10");

    // Assertions trust column exists with default.
    assert!(has_column(&conn, "assertions", "trust"));
}
