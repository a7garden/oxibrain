//! One writer per store (P8). Cross-process advisory lock, fail-fast (DESIGN §4.3).

use crate::io_err;
use fs2::FileExt;
use oxibrain_ports::BrainError;
use std::fs::{File, OpenOptions};
use std::path::Path;

pub struct AdvisoryLock {
    _file: File,
}

impl AdvisoryLock {
    /// Acquire an exclusive lock on `<dir>/.oxibrain.lock`. Fails fast with
    /// `BrainError::Locked` if another oxibrain process holds it (no blocking).
    pub fn acquire(dir: &Path) -> Result<Self, BrainError> {
        std::fs::create_dir_all(dir).map_err(io_err)?;
        let lock_path = dir.join(".oxibrain.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(io_err)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { _file: file }),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(BrainError::Locked {
                holder: format!("another oxibrain process holds {}", lock_path.display()),
            }),
            Err(e) => Err(io_err(e)),
        }
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}
