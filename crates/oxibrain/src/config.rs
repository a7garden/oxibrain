use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct BrainConfig {
    pub dir: PathBuf,
    pub readers: usize,
}

impl BrainConfig {
    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            dir: path.as_ref().to_path_buf(),
            readers: 4,
        }
    }
}
