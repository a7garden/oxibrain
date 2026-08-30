//! Canonical Oxi installation paths used by the oxibrain library.

use std::path::{Path, PathBuf};

/// Resolve the shared Oxi home. `OXI_HOME` is intended for tests and portable
/// deployments; the normal default is the user's `~/.oxi` directory.
pub fn oxi_home() -> PathBuf {
    if let Some(path) = std::env::var_os("OXI_HOME") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".oxi")
}

/// Resolve the canonical oxibrain data directory.
pub fn brain_dir() -> PathBuf {
    brain_dir_from_home(&oxi_home())
}

/// Resolve a brain directory below an already-resolved Oxi home.
pub fn brain_dir_from_home(home: &Path) -> PathBuf {
    home.join("brain")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn brain_dir_is_nested_under_oxi_home() {
        assert_eq!(
            super::brain_dir_from_home(Path::new("/tmp/oxi-test")),
            Path::new("/tmp/oxi-test/brain")
        );
    }

    #[test]
    fn oxi_home_is_not_app_specific() {
        assert_eq!(
            oxi_home_from_env(Some("/tmp/oxi"), "/tmp/home"),
            Path::new("/tmp/oxi")
        );
        assert_eq!(
            oxi_home_from_env(None, "/tmp/home"),
            Path::new("/tmp/home/.oxi")
        );
    }

    fn oxi_home_from_env(override_home: Option<&str>, user_home: &str) -> PathBuf {
        override_home
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(user_home).join(".oxi"))
    }
}
