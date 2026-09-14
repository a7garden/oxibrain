//! `documents.toml` — the plain-text declaration of which on-disk roots the
//! document plane should reconcile.
//!
//! The shape and the rules below mirror `docs/superpowers/specs/2026-08-27-…`
//! §4.1 verbatim: every root entry carries an alias (unique identifier inside
//! this file), a filesystem `path` (with `~` expanded at load), the target
//! `space`, an include and exclude glob list, and a size limit. Defaults match
//! the spec so an empty section behaves sensibly:
//!
//! ```toml
//! [[root]]
//! alias = "vault"
//! path = "~/.oxi/vault"
//! space = "personal"
//! include = ["**/*.md", "**/*.txt", "**/*.html"]
//! exclude = ["**/.git/**", "**/.DS_Store", "**/*.tmp", "**/*.lock"]
//! max_file_bytes = 10485760
//! ```
//!
//! Loaded from `<dir>/documents.toml`. Missing files are not an error: they
//! produce an empty configuration so the first run on a fresh brain is harmless
//! (operators seed the file intentionally via `oxibrain init`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

const DEFAULT_INCLUDE: &[&str] = &["**/*.md", "**/*.txt", "**/*.html", "**/*.djot", "**/*.base"];
const DEFAULT_EXCLUDE: &[&str] = &["**/.git/**", "**/.DS_Store", "**/*.tmp", "**/*.lock"];
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Uniqueness suffix for atomic-save temp files: pid alone collides when
/// two saves run concurrently in one process (the first rename would move
/// the shared temp file out from under the second save).
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// File-name of the on-disk configuration inside the brain directory.
pub const CONFIG_FILE_NAME: &str = "documents.toml";

/// One declared root (alias + on-disk location + matching rules).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootEntry {
    pub alias: String,
    /// Filesystem path. A leading `~` expands to `$HOME` at load time.
    pub path: PathBuf,
    /// Logical space this root writes into.
    pub space: String,
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
}

/// Outcome of an idempotent [`DocumentsConfig::upsert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// No entry existed for the alias; it was appended.
    Added,
    /// The alias existed with different rules; the entry was replaced.
    Replaced,
    /// The alias existed with byte-identical rules; nothing changed.
    Unchanged,
}

/// The complete configuration: an ordered list of root entries plus the
/// surrounding metadata we may grow later (default space, etc.).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentsConfig {
    #[serde(rename = "root", default)]
    pub roots: Vec<RootEntry>,
}

/// Errors surfaced by [`DocumentsConfig`] load/save/validate.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("parse error in {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("io error: {0}")]
    Io(String),
}

impl DocumentsConfig {
    /// Load `<dir>/documents.toml`. Missing file ⇒ empty config (not an error).
    ///
    /// The `~` prefix on each root's `path` is expanded against the calling
    /// process's `$HOME` before returning; an unset `$HOME` leaves `~`
    /// untouched so the caller can surface a helpful error at validate time.
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join(CONFIG_FILE_NAME);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConfigError::Io(e.to_string())),
        };
        let mut cfg: Self = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path,
            message: e.to_string(),
        })?;
        for root in &mut cfg.roots {
            expand_tilde(&mut root.path);
        }
        Ok(cfg)
    }

    /// Persist the configuration to `<dir>/documents.toml`. Creates the
    /// directory if missing so callers can hand us a fresh brain dir.
    ///
    /// The write is atomic (temp file + rename in the same directory) so a
    /// crash mid-save can never leave a truncated or half-written config
    /// behind — every registration rides this path.
    pub fn save(dir: &Path, cfg: &DocumentsConfig) -> Result<(), ConfigError> {
        if let Err(e) = fs::create_dir_all(dir) {
            return Err(ConfigError::Io(e.to_string()));
        }
        let path = dir.join(CONFIG_FILE_NAME);
        let text = toml::to_string_pretty(cfg).map_err(|e| ConfigError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}-{}",
            CONFIG_FILE_NAME,
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(e) = fs::write(&tmp, &text) {
            return Err(ConfigError::Io(e.to_string()));
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(ConfigError::Io(e.to_string()));
        }
        Ok(())
    }

    /// Reject configurations that would never reconcile coherently:
    /// duplicates aliases, empty required fields, etc.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::BTreeSet::new();
        for root in &self.roots {
            if root.alias.is_empty() {
                return Err(ConfigError::Invalid("root.alias must not be empty".into()));
            }
            if root.space.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "root.{}: space must not be empty",
                    root.alias
                )));
            }
            if root.path.as_os_str().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "root.{}: path must not be empty",
                    root.alias
                )));
            }
            if root.max_file_bytes == 0 {
                return Err(ConfigError::Invalid(format!(
                    "root.{}: max_file_bytes must be > 0",
                    root.alias
                )));
            }
            if !seen.insert(root.alias.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate root alias {:?}",
                    root.alias
                )));
            }
        }
        Ok(())
    }

    /// Alias lookup — the read side of [`Self::upsert`]'s key.
    pub fn root(&self, alias: &str) -> Option<&RootEntry> {
        self.roots.iter().find(|r| r.alias == alias)
    }

    /// Idempotent upsert keyed by `alias`: append the entry when the alias
    /// is new, replace it in place when its rules differ, and report
    /// [`UpsertOutcome::Unchanged`] when an identical entry already exists.
    ///
    /// Duplicate aliases (legacy pollution from the flat era — several
    /// `[[root]]` blocks sharing one alias) are collapsed to the single
    /// incoming entry: `validate()` rejects duplicate aliases, so without
    /// the collapse a polluted config could never be repaired through the
    /// registration boundary. This is the pure decision behind the facade's
    /// `register_document_root` operation — duplicate and replacement
    /// semantics live here and nowhere else, so every caller (facade,
    /// server, tests) agrees on them.
    pub fn upsert(&mut self, entry: RootEntry) -> UpsertOutcome {
        let mut kept_identical = false;
        let mut had_alias = false;
        self.roots.retain(|existing| {
            if existing.alias != entry.alias {
                return true;
            }
            had_alias = true;
            if !kept_identical && *existing == entry {
                kept_identical = true;
                return true;
            }
            false
        });
        if kept_identical {
            return UpsertOutcome::Unchanged;
        }
        self.roots.push(entry);
        if had_alias {
            UpsertOutcome::Replaced
        } else {
            UpsertOutcome::Added
        }
    }

    /// Collapse duplicate-alias entries, keeping the FIRST occurrence
    /// per alias (the operator-provisioned original; later blocks are
    /// pollution). Returns the number of entries dropped. Pure repair
    /// toward the `validate()` invariant: without it, a polluted config
    /// could never be brought back in bounds through the registration
    /// boundary, because `validate()` runs on the whole file.
    pub fn dedupe(&mut self) -> usize {
        let before = self.roots.len();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.roots.retain(|root| seen.insert(root.alias.clone()));
        before - self.roots.len()
    }
}

/// Connector-default include globs, exposed so the facade's registration
/// op fills omitted rules from the same source of truth as serde defaults.
pub fn default_include() -> Vec<String> {
    DEFAULT_INCLUDE.iter().map(|s| (*s).to_string()).collect()
}

/// Connector-default exclude globs (see [`default_include`]).
pub fn default_exclude() -> Vec<String> {
    DEFAULT_EXCLUDE.iter().map(|s| (*s).to_string()).collect()
}

/// Connector-default per-file byte cap (see [`default_include`]).
pub fn default_max_file_bytes() -> u64 {
    DEFAULT_MAX_FILE_BYTES
}

/// Expand a leading `~` (or `~/…`) against `$HOME`. Anything else is left
/// untouched so a relative or absolute path passes through verbatim.
fn expand_tilde(path: &mut PathBuf) {
    let s = path.to_string_lossy();
    let stripped = match s.strip_prefix("~/") {
        Some(rest) => rest.to_string(),
        None => match s.as_ref() {
            "~" => String::new(),
            _ => return,
        },
    };
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let mut combined = PathBuf::from(home);
    if !stripped.is_empty() {
        combined.push(stripped);
    }
    *path = combined;
}
