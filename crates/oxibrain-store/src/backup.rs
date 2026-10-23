//! Online backup (WAL-safe) via SQLite's backup API. ARCHITECTURE.md §16.5.

use crate::io_err;
use crate::schema::LEDGER_SCHEMA_VERSION;
use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, OpenFlags, backup};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupManifest {
    pub ledger_schema_version: i64,
    pub projection_version: i64,
    pub include_projection: bool,
    pub include_cache: bool,
    pub created_at: i64,
}

/// Back up the source connection's main db into `dest_path` using the online API.
///
/// Backup is initiated from the source (rusqlite's `Backup::new_with_names`
/// borrows `from` and `to` together; the source handles the page transfer).
pub fn online_backup(src: &Connection, dest_path: &Path) -> Result<(), BrainError> {
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let mut dest = Connection::open_with_flags(
        dest_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(sql_err)?;
    // src → dest: API is initiated from the source connection (it holds the
    // borrow that drives `sqlite3_backup_step`). The plan's intent maps onto
    // `backup::Backup::new_with_names(src, DatabaseName::Main, dest, Main)`.
    let bkp = backup::Backup::new_with_names(
        src,
        rusqlite::DatabaseName::Main,
        &mut dest,
        rusqlite::DatabaseName::Main,
    )
    .map_err(sql_err)?;
    bkp.run_to_completion(100, std::time::Duration::from_millis(10), None)
        .map_err(sql_err)?;
    Ok(())
}

/// Inspect a standalone snapshot without migrating or changing it.
///
/// A backup artifact is accepted only when SQLite's full integrity and foreign
/// key checks pass. `user_version` is checked separately so an empty SQLite
/// file cannot be mistaken for a valid brain.
pub fn inspect_snapshot(path: &Path) -> Result<(i64, i64), BrainError> {
    crate::migration::ensure_vec_extension();
    let conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_err)?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(sql_err)?;
    if integrity != "ok" {
        return Err(BrainError::Corruption(format!(
            "snapshot integrity check failed: {integrity}"
        )));
    }
    let foreign_key_violation: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(sql_err)?;
    if foreign_key_violation != 0 {
        return Err(BrainError::Corruption(format!(
            "snapshot has {foreign_key_violation} foreign-key violation(s)"
        )));
    }
    let schema: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_err)?;
    if !(1..=LEDGER_SCHEMA_VERSION).contains(&schema) {
        return Err(BrainError::Migration {
            found: schema,
            expected: LEDGER_SCHEMA_VERSION,
        });
    }
    let projection: i64 = crate::meta::get(&conn, "projection_version")?
        .ok_or_else(|| BrainError::Corruption("snapshot lacks projection_version".into()))?
        .parse()
        .map_err(|_| BrainError::Corruption("invalid projection_version".into()))?;
    Ok((schema, projection))
}

/// Remove selected rebuildable rows from a *detached snapshot* only.
///
/// The CLI calls this after the online backup has completed. The live source
/// connection is never passed here. Omitting extraction cache also omits the
/// projection, because its extracted claims cannot be rebuilt from the ledger
/// until extraction runs again.
pub fn prune_snapshot(path: &Path, no_projection: bool, no_cache: bool) -> Result<(), BrainError> {
    if !no_projection && !no_cache {
        return Ok(());
    }
    crate::migration::ensure_vec_extension();
    let mut conn = Connection::open(path).map_err(sql_err)?;
    let schema: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_err)?;
    if schema != LEDGER_SCHEMA_VERSION {
        return Err(BrainError::Migration {
            found: schema,
            expected: LEDGER_SCHEMA_VERSION,
        });
    }
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=OFF;")
        .map_err(sql_err)?;
    let transaction = conn.transaction().map_err(sql_err)?;
    if no_projection || no_cache {
        // Truth and ranking projections. Registry data is retained: custom
        // predicate definitions and source policies are needed by replay.
        for table in [
            "beliefs",
            "mentions",
            "assertions",
            "statements",
            "entity_merges",
            "entity_keys",
            "entities",
            "communities",
            "chunks",
            "entity_vectors",
            "tfidf_vectors",
            "fts_map",
            "memory_source_chunks",
            "memory_chunk_fts_word",
            "memory_chunk_fts_ngram",
            "fts_word",
            "fts_ngram",
            "consolidation_checkpoints",
        ] {
            transaction
                .execute(&format!("DELETE FROM {table}"), [])
                .map_err(sql_err)?;
        }
    }
    if no_cache {
        for table in ["extractions", "summaries", "extraction_failures"] {
            transaction
                .execute(&format!("DELETE FROM {table}"), [])
                .map_err(sql_err)?;
        }
    }
    transaction.commit().map_err(sql_err)?;
    conn.execute_batch("PRAGMA foreign_keys=ON; VACUUM;")
        .map_err(sql_err)?;
    drop(conn);
    inspect_snapshot(path)?;
    Ok(())
}
