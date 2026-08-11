//! Store: the only crate that touches rusqlite.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod backup;
pub mod communities;
pub mod consolidation;
pub mod context;
pub mod explain;
pub mod export;
pub mod extraction;
pub mod index_ops;
pub mod knowledge;
pub mod ledger;
pub mod lifecycle;
pub mod lock;
pub mod meta;
pub mod migration;
pub mod project;
pub mod quarantine;
pub mod query;
pub mod reader;
pub mod registry;
pub mod reproject;
pub mod schema;
pub mod timeline;
pub mod writer;
pub mod redaction;
pub mod security;
pub use security::{AuditRow, list_audit, write_audit};

pub use backup::{BackupManifest, online_backup};
pub use project::{
    DeclObject, Declaration, EntityRef, canonical_declaration_content, parse_declaration,
    project_declaration,
};
pub use reader::ReaderPool;
pub use reproject::reproject;
pub use writer::WriterActor;

use oxibrain_ports::BrainError;
use std::path::{Path, PathBuf};

/// Owns the writer actor and reader pool. The facade wraps this async.
pub struct StoreHandle {
    pub writer: WriterActor,
    pub readers: ReaderPool,
    pub db_path: PathBuf,
}

impl StoreHandle {
    pub fn open(dir: &Path) -> Result<Self, BrainError> {
        let store = Store::open(dir)?;
        let db_path = store.db_path().to_path_buf();
        let readers = ReaderPool::open(&db_path, 4)?;
        let writer = WriterActor::spawn(store);
        Ok(Self {
            writer,
            readers,
            db_path,
        })
    }
}

pub struct Store {
    pub(crate) write_conn: rusqlite::Connection,
    pub(crate) path: PathBuf,
    _lock: lock::AdvisoryLock,
}

/// Convert a rusqlite error into a BrainError. Store-local by necessity: a blanket
/// `From<rusqlite::Error> for BrainError` would violate the orphan rule (BrainError is
/// foreign to this crate, and so is rusqlite). Use `.map_err(sql_err)?` at every rusqlite
/// boundary; the `?`-on-rusqlite shortcut does not compile here.
pub(crate) fn sql_err(e: rusqlite::Error) -> BrainError {
    BrainError::Storage(e.to_string())
}
pub(crate) fn io_err(e: std::io::Error) -> BrainError {
    BrainError::Storage(e.to_string())
}

impl Store {
    /// Open (or create) a store at `dir`. Acquires the advisory lock, applies migrations,
    /// sets PRAGMAs, and seeds meta versions.
    pub fn open(dir: &Path) -> Result<Self, BrainError> {
        let lock = lock::AdvisoryLock::acquire(dir)?;
        let db_path = dir.join("brain.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let write_conn = rusqlite::Connection::open(&db_path).map_err(sql_err)?;
        for p in schema::PRAGMAS {
            write_conn.execute_batch(p).map_err(sql_err)?;
        }
        migration::run(&write_conn)?;
        meta::ensure_schema_versions(&write_conn)?;
        Ok(Self {
            write_conn,
            path: db_path,
            _lock: lock,
        })
    }

    pub fn user_version(&self) -> Result<i64, BrainError> {
        self.write_conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sql_err)
    }

    /// Read-only handle to the write connection (backup, doctor). Writes go through the actor.
    pub fn connection(&self) -> &rusqlite::Connection {
        &self.write_conn
    }

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    /// Move the write connection and advisory lock out of the store. The writer actor
    /// holds both so the lock lives for the actor's lifetime (P8). Only callable in-crate.
    pub(crate) fn into_parts(self) -> (rusqlite::Connection, lock::AdvisoryLock) {
        let Store {
            write_conn,
            path: _,
            _lock,
        } = self;
        (write_conn, _lock)
    }
}
