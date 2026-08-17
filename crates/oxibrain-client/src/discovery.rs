//! Stable daemon discovery for Oxi Foundation (spec §1, §8).
//!
//! Resolves the canonical socket location for the local daemon:
//!
//! 1. `$OXIBRAIN_SOCKET` if set (explicit override).
//! 2. `$HOME/.oxi/brain/oxibrain.sock` otherwise (canonical default).
//!
//! `default_socket_path()` does **not** consult the filesystem and does **not**
//! create directories — that is the daemon's job. Client-side lookups are
//! pure path arithmetic so that callers can decide how to react to a missing
//! daemon (`BrainClient::connect_default` will fail fast with a typed error).
//!
//! A `BrainEndpoint` is an absolute, non-empty path validated at construction.
//! Relative paths, empty paths, and paths containing null bytes are rejected
//! with a typed error.

use std::env;
use std::path::{Path, PathBuf};

/// Canonical default socket name (relative to `$HOME/.oxi/brain/`).
pub const DEFAULT_SOCKET_FILENAME: &str = "oxibrain.sock";

/// Environment variable that overrides the canonical default.
pub const OXIBRAIN_SOCKET_ENV: &str = "OXIBRAIN_SOCKET";

/// Environment variable that overrides the user's home directory (test hook;
/// matches `oxibrain-cli`'s internal helper).
pub const HOME_ENV: &str = "HOME";

/// Resolve the default daemon socket path.
///
/// Returns `$OXIBRAIN_SOCKET` if set (the env var is the explicit override
/// described in Foundation spec §1). Otherwise returns
/// `$HOME/.oxi/brain/oxibrain.sock`. Never creates directories.
///
/// If neither variable is set (e.g. in a sandboxed test) and `$HOME` is
/// unset, returns the path relative to an empty prefix — the caller will see
/// the relative path and either reject it or treat it as "no default".
/// Tests that need a known absolute path should set `$HOME` (or use
/// `BrainEndpoint::from_path` directly).
pub fn default_socket_path() -> Option<PathBuf> {
    if let Some(p) = env::var_os(OXIBRAIN_SOCKET_ENV) {
        let buf = PathBuf::from(p);
        if !buf.as_os_str().is_empty() {
            return Some(buf);
        }
    }
    let home = env::var_os(HOME_ENV)?;
    let mut buf = PathBuf::from(home);
    buf.push(".oxi");
    buf.push("brain");
    buf.push(DEFAULT_SOCKET_FILENAME);
    Some(buf)
}

/// Validated, absolute socket path. Constructed via [`BrainEndpoint::default`]
/// or [`BrainEndpoint::from_path`]; both reject malformed input.
///
/// The endpoint is intentionally trivial: it is the discovery contract that
/// other hosts parse. New fields belong on the handshake
/// ([`crate::protocol::ServerInfo`]) — not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainEndpoint {
    path: PathBuf,
}

/// Errors a caller can see from the discovery surface.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("socket path must be absolute (got {path:?})")]
    NotAbsolute { path: PathBuf },
    #[error("socket path is empty")]
    Empty,
}

impl BrainEndpoint {
    /// The default endpoint (resolved via [`default_socket_path`]).
    ///
    /// Returns an error if neither `$OXIBRAIN_SOCKET` nor `$HOME` is set or if
    /// the resolved path is not absolute.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Result<Self, DiscoveryError> {
        let path = default_socket_path().ok_or(DiscoveryError::Empty)?;
        Self::from_path(path)
    }

    /// Construct an endpoint from an explicit path. Rejects empty paths and
    /// relative paths.
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, DiscoveryError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(DiscoveryError::Empty);
        }
        if !path.is_absolute() {
            return Err(DiscoveryError::NotAbsolute { path });
        }
        Ok(Self { path })
    }

    /// The validated socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consume the endpoint, returning the underlying path.
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

impl AsRef<Path> for BrainEndpoint {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests in this module mutate process-global env vars. Run them
    /// sequentially so a stale var from one test cannot pollute another.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn endpoint_rejects_empty_path() {
        let err = BrainEndpoint::from_path("").unwrap_err();
        matches!(err, DiscoveryError::Empty);
    }

    #[test]
    fn endpoint_rejects_relative_path() {
        let err = BrainEndpoint::from_path("./oxibrain.sock").unwrap_err();
        match err {
            DiscoveryError::NotAbsolute { path } => {
                assert!(path.ends_with("oxibrain.sock"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn endpoint_accepts_absolute_path() {
        let ep = BrainEndpoint::from_path("/tmp/oxibrain.sock").unwrap();
        assert_eq!(ep.path(), Path::new("/tmp/oxibrain.sock"));
    }

    #[test]
    fn env_override_takes_precedence_over_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Save and restore the env so this test is hermetic.
        let prev_socket = env::var_os(OXIBRAIN_SOCKET_ENV);
        let prev_home = env::var_os(HOME_ENV);

        // SAFETY: tests in this module run on a single thread by default.
        unsafe {
            env::set_var(OXIBRAIN_SOCKET_ENV, "/var/run/oxibrain.sock");
            env::set_var(HOME_ENV, "/home/test");
        }
        let path = default_socket_path().expect("present");
        assert_eq!(path, PathBuf::from("/var/run/oxibrain.sock"));

        unsafe {
            env::set_var(OXIBRAIN_SOCKET_ENV, "/tmp/overridden.sock");
        }
        let path = default_socket_path().expect("present");
        assert_eq!(path, PathBuf::from("/tmp/overridden.sock"));

        // Clean up.
        match prev_socket {
            Some(v) => unsafe { env::set_var(OXIBRAIN_SOCKET_ENV, v) },
            None => unsafe { env::remove_var(OXIBRAIN_SOCKET_ENV) },
        }
        match prev_home {
            Some(v) => unsafe { env::set_var(HOME_ENV, v) },
            None => unsafe { env::remove_var(HOME_ENV) },
        }
    }

    #[test]
    fn unset_socket_falls_back_to_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_socket = env::var_os(OXIBRAIN_SOCKET_ENV);
        let prev_home = env::var_os(HOME_ENV);

        unsafe {
            env::remove_var(OXIBRAIN_SOCKET_ENV);
            env::set_var(HOME_ENV, "/home/fallback");
        }
        let path = default_socket_path().expect("present");
        assert_eq!(
            path,
            PathBuf::from("/home/fallback/.oxi/brain/oxibrain.sock")
        );

        match prev_socket {
            Some(v) => unsafe { env::set_var(OXIBRAIN_SOCKET_ENV, v) },
            None => unsafe { env::remove_var(OXIBRAIN_SOCKET_ENV) },
        }
        match prev_home {
            Some(v) => unsafe { env::set_var(HOME_ENV, v) },
            None => unsafe { env::remove_var(HOME_ENV) },
        }
    }

    #[test]
    fn endpoint_default_matches_default_socket_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_socket = env::var_os(OXIBRAIN_SOCKET_ENV);
        let prev_home = env::var_os(HOME_ENV);

        unsafe {
            env::set_var(OXIBRAIN_SOCKET_ENV, "/opt/oxibrain/test.sock");
            env::remove_var(HOME_ENV);
        }
        let ep = BrainEndpoint::default().expect("present");
        assert_eq!(ep.path(), Path::new("/opt/oxibrain/test.sock"));

        match prev_socket {
            Some(v) => unsafe { env::set_var(OXIBRAIN_SOCKET_ENV, v) },
            None => unsafe { env::remove_var(OXIBRAIN_SOCKET_ENV) },
        }
        match prev_home {
            Some(v) => unsafe { env::set_var(HOME_ENV, v) },
            None => unsafe { env::remove_var(HOME_ENV) },
        }
    }
}
