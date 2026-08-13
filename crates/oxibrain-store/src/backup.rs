//! Online backup (WAL-safe) via SQLite's backup API. ARCHITECTURE.md §16.5.

use crate::io_err;
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
