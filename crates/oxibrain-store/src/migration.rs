//! Forward-only migrations via PRAGMA user_version. Every migration has an up-test.

use crate::schema::LEDGER_SCHEMA_VERSION;
use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::Connection;

/// Register the sqlite-vec extension globally. Must be called before opening
/// any connection that will use vec0 virtual tables. Idempotent via `Once`.
static VEC_REGISTERED: std::sync::Once = std::sync::Once::new();

/// Ensure the sqlite-vec extension is registered for all future connections.
/// Call this before opening a connection.
#[allow(clippy::missing_transmute_annotations)]
pub fn ensure_vec_extension() {
    VEC_REGISTERED.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}
/// Apply all pending migrations. Returns the new user_version.
pub fn run(conn: &Connection) -> Result<i64, BrainError> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_err)?;
    if current > LEDGER_SCHEMA_VERSION {
        return Err(BrainError::Migration {
            found: current,
            expected: LEDGER_SCHEMA_VERSION,
        });
    }
    if current < 1 {
        let sql = include_str!("migrations/v1.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 1i64)
            .map_err(sql_err)?;
    }
    if current < 2 {
        let sql = include_str!("migrations/v2.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        // Seed core/v1 predicates (data migration, not schema).
        crate::registry::seed_core_v1(conn)?;
        conn.pragma_update(None, "user_version", 2i64)
            .map_err(sql_err)?;
    }
    if current < 3 {
        let sql = include_str!("migrations/v3.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 3i64)
            .map_err(sql_err)?;
    }
    if current < 4 {
        let sql = include_str!("migrations/v4.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 4i64)
            .map_err(sql_err)?;
    }
    if current < 5 {
        ensure_vec_extension();
        let sql = include_str!("migrations/v5.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 5i64)
            .map_err(sql_err)?;
    }
    if current < 6 {
        let sql = include_str!("migrations/v6.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 6i64)
            .map_err(sql_err)?;
    }
    if current < 7 {
        let sql = include_str!("migrations/v7.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 7i64)
            .map_err(sql_err)?;
    }
    if current < 8 {
        let sql = include_str!("migrations/v8.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 8i64)
            .map_err(sql_err)?;
    }
    if current < 9 {
        let sql = include_str!("migrations/v9.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 9i64)
            .map_err(sql_err)?;
    }
    if current < 10 {
        let sql = include_str!("migrations/v10.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 10i64)
            .map_err(sql_err)?;
    }
    if current < 11 {
        let sql = include_str!("migrations/v11.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 11i64)
            .map_err(sql_err)?;
    }
    if current < 12 {
        ensure_vec_extension();
        // Convert existing float32 rows to int8 BEFORE the DDL drops the
        // vec0 table (ADR-014: ranking-half state, converted in place so
        // no re-embedding pass is required after upgrade).
        let old: Vec<(String, Vec<u8>)> = {
            let mut stmt = conn
                .prepare("SELECT entity_id, embedding FROM entity_vectors")
                .map_err(sql_err)?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?
        };
        let sql = include_str!("migrations/v12.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        for (entity_id, blob) in &old {
            let floats: Vec<f32> = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let q = oxibrain_index::quantize_i8_unit(&floats);
            conn.execute(
                "INSERT INTO entity_vectors(entity_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![entity_id, q],
            )
            .map_err(sql_err)?;
        }
        conn.pragma_update(None, "user_version", 12i64)
            .map_err(sql_err)?;
    }
    if current < 13 {
        let sql = include_str!("migrations/v13.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        // Repopulate both contentless indexes from the ledger (pure SQL +
        // tokenization, no model calls). Same three passes as rebuild_fts.
        let spaces: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM spaces").map_err(sql_err)?;
            stmt.query_map([], |r| r.get(0))
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?
        };
        for space in &spaces {
            crate::index_ops::rebuild_fts(conn, space)?;
        }
        conn.pragma_update(None, "user_version", 13i64)
            .map_err(sql_err)?;
    }
    let now: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_err)?;
    Ok(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn fresh_db_migrates_to_current() {
        ensure_vec_extension();
        let conn = Connection::open_in_memory().expect("open");
        let v = run(&conn).expect("migrate");
        assert_eq!(v, LEDGER_SCHEMA_VERSION);
        // spot-check a table exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
            .expect("query");
        assert_eq!(count, 0);
        // predicates were seeded
        let pred_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM predicates", [], |r| r.get(0))
            .expect("query");
        assert!(pred_count > 0, "core/v1 predicates should be seeded");
        // v5: vec0 virtual table exists
        let vec_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entity_vectors", [], |r| r.get(0))
            .expect("vec0 table query");
        assert_eq!(vec_count, 0);
        // v9: uncertainty_json column exists on episodes
        let has_col: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('episodes') WHERE name = 'uncertainty_json'",
                [],
                |r| r.get(0),
            )
            .expect("pragma query");
        assert_eq!(has_col, 1, "uncertainty_json column should exist after v9");
    }

    #[test]
    fn newer_db_is_hard_error() {
        ensure_vec_extension();
        let conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "user_version", 999i64)
            .expect("set");
        let err = run(&conn).unwrap_err();
        assert!(matches!(
            err,
            BrainError::Migration {
                found: 999,
                expected: LEDGER_SCHEMA_VERSION
            }
        ));
    }
}
