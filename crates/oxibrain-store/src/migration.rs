//! Forward-only migrations via PRAGMA user_version. Every migration has an up-test.

use crate::schema::LEDGER_SCHEMA_VERSION;
use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::Connection;

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
    }

    #[test]
    fn newer_db_is_hard_error() {
        let conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "user_version", 999i64)
            .expect("set");
        let err = run(&conn).unwrap_err();
        assert!(matches!(
            err,
            BrainError::Migration {
                found: 999,
                expected: 3
            }
        ));
    }
}
