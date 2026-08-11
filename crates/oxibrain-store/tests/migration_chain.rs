//! Migration chain tests from every historical schema version (AGENTS.md).
//!
//! Each test creates a DB at a specific schema version with test data, runs the
//! migration to current, and verifies data integrity + new structures.

use oxibrain_store::{migration, registry, schema::LEDGER_SCHEMA_VERSION};
use rusqlite::Connection;

const V1_SQL: &str = include_str!("../src/migrations/v1.sql");
const V2_SQL: &str = include_str!("../src/migrations/v2.sql");

/// Insert minimal test data: one space, one episode.
fn insert_test_data(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO spaces (id, name, created_at) VALUES ('sp1', 'test', 1000);
         INSERT INTO episodes
           (id, space_id, seq, content_hash, content, source_kind, source_ref,
            trust, kind, occurred_at, ingested_at)
         VALUES ('ep1', 'sp1', 0, x'00', 'test content', 'note', 'test.md',
                 'trusted', 'primary', 1000, 1000);",
    )
    .expect("insert test data");
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

// ── Fresh DB → current (original test) ───────────────────────────────────────

#[test]
fn migrates_from_empty_to_current() {
    let conn = Connection::open_in_memory().unwrap();
    // simulate a pre-migration db
    conn.execute_batch("CREATE TABLE spaces(id TEXT);").unwrap();
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    let _n: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
}

// ── v1 → current (v3) ───────────────────────────────────────────────────────

#[test]
fn migrates_from_v1_with_data() {
    let conn = Connection::open_in_memory().unwrap();
    // Apply v1 schema only.
    conn.execute_batch(V1_SQL).unwrap();
    conn.pragma_update(None, "user_version", 1i64).unwrap();
    insert_test_data(&conn);

    // Migrate.
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);

    // ── Data integrity: episode survived the full migration chain. ──
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "episode must survive migration");
    let content: String = conn
        .query_row("SELECT content FROM episodes WHERE id = 'ep1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(content, "test content");

    // ── v2 effects: predicates seeded (data migration). ──
    let pred_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM predicates", [], |r| r.get(0))
        .unwrap();
    assert!(pred_count > 0, "v2 migration must seed core/v1 predicates");

    // ── v2 effects: mentions table fixed (id no longer FK to assertions). ──
    // The v2 migration drops and recreates mentions without the spurious FK.
    assert!(has_table(&conn, "mentions"));

    // ── v3 effects: FTS5 virtual table. ──
    assert!(
        has_table(&conn, "episodes_fts"),
        "v3 migration must create FTS5 table"
    );

    // ── v3 effects: TF-IDF vectors table. ──
    assert!(
        has_table(&conn, "tfidf_vectors"),
        "v3 migration must create tfidf_vectors table"
    );

    // ── v3 effects: salience columns on entities. ──
    assert!(
        has_column(&conn, "entities", "salience"),
        "v3 migration must add salience column"
    );
    assert!(
        has_column(&conn, "entities", "last_activity"),
        "v3 migration must add last_activity column"
    );

    // ── v3 effects: compaction columns on episodes. ──
    assert!(
        has_column(&conn, "episodes", "content_compacted"),
        "v3 migration must add content_compacted column"
    );
    assert!(
        has_column(&conn, "episodes", "compacted_at"),
        "v3 migration must add compacted_at column"
    );
}

// ── v2 → current (v3) ───────────────────────────────────────────────────────

#[test]
fn migrates_from_v2_with_data() {
    let conn = Connection::open_in_memory().unwrap();
    // Apply v1 + v2 schema + predicate seed.
    conn.execute_batch(V1_SQL).unwrap();
    conn.execute_batch(V2_SQL).unwrap();
    registry::seed_core_v1(&conn).unwrap();
    conn.pragma_update(None, "user_version", 2i64).unwrap();
    insert_test_data(&conn);

    // Record predicate count before migration.
    let preds_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM predicates", [], |r| r.get(0))
        .unwrap();
    assert!(preds_before > 0, "v2 setup must have seeded predicates");

    // Migrate (only v3 applies).
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);

    // ── Data integrity. ──
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    // ── v2 data preserved: predicates not re-seeded. ──
    let preds_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM predicates", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        preds_before, preds_after,
        "v3 migration must not re-seed predicates"
    );

    // ── v3 effects: all new structures. ──
    assert!(has_table(&conn, "episodes_fts"));
    assert!(has_table(&conn, "tfidf_vectors"));
    assert!(has_column(&conn, "entities", "salience"));
    assert!(has_column(&conn, "episodes", "content_compacted"));
    assert!(has_column(&conn, "episodes", "compacted_at"));
}

// ── Idempotency ──────────────────────────────────────────────────────────────

#[test]
fn migration_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    let v1: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v1, LEDGER_SCHEMA_VERSION);

    // Running again must be a no-op.
    migration::run(&conn).unwrap();
    let v2: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v1, v2, "re-running migration must be a no-op");
}

// ── Future version guard ─────────────────────────────────────────────────────

#[test]
fn newer_db_is_hard_error() {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", 999i64).unwrap();
    let err = migration::run(&conn).unwrap_err();
    use oxibrain_ports::BrainError;
    assert!(matches!(
        err,
        BrainError::Migration {
            found: 999,
            expected: LEDGER_SCHEMA_VERSION,
        }
    ));
}
