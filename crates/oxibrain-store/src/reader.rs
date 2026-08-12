//! Reader pool: N read-only WAL connections. Readers never block on the writer.

use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

pub struct ReaderPool {
    conns: Vec<Mutex<Connection>>,
}

impl ReaderPool {
    pub fn open(db_path: &Path, size: usize) -> Result<Self, BrainError> {
        crate::migration::ensure_vec_extension();
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            // open a *new* connection that shares the db file; read-only via query discipline
            let conn = Connection::open(db_path).map_err(sql_err)?;
            conn.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")
                .map_err(sql_err)?;
            conns.push(Mutex::new(conn));
        }
        Ok(Self { conns })
    }

    /// Run a read closure on the next available connection (round-robin / first-free).
    pub fn read<R>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<R, BrainError>,
    ) -> Result<R, BrainError> {
        for m in &self.conns {
            if let Ok(guard) = m.try_lock() {
                return f(&guard);
            }
        }
        // all busy: block on the first
        let guard = self.conns[0]
            .lock()
            .map_err(|_| BrainError::Storage("reader mutex poisoned".into()))?;
        f(&guard)
    }
}
