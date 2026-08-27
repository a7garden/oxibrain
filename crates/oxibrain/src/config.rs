use std::path::{Path, PathBuf};

/// Runtime configuration for a [`crate::Brain`]. Carries only the store
/// directory and open mode — the handle-free facade re-opens the store per
/// operation, so there is no pool size or actor knob to tune anymore
/// (`readers` is kept for source compatibility and ignored).
#[derive(Debug, Clone)]
pub struct BrainConfig {
    pub dir: PathBuf,
    /// Ignored since the handle-free rework; kept so existing struct
    /// literals keep compiling.
    pub readers: usize,
    /// When true, every write path fails fast with a Config error instead
    /// of taking the advisory lock. Set by [`BrainConfig::read_only_at`];
    /// [`Brain::open_ro`](crate::Brain::open_ro) uses it.
    pub read_only: bool,
}

impl BrainConfig {
    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            dir: path.as_ref().to_path_buf(),
            readers: 4,
            read_only: false,
        }
    }

    /// Read-only variant of [`BrainConfig::at`]: writes are rejected without
    /// touching the advisory lock.
    pub fn read_only_at(path: impl AsRef<Path>) -> Self {
        Self {
            read_only: true,
            ..Self::at(path)
        }
    }
}
