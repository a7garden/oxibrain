//! Per-space vault provisioning (spec §4.3).
//!
//! `provision_space_vault` is the shared body used by `space add` and any
//! other entry point that creates a brand-new space. The function decides
//! nothing about which brain dir or which home to operate on — the caller
//! (the CLI arm) has already resolved both. The function is idempotent on
//! every step: running it twice on the same `(brain_dir, home, name)`
//! produces the same final state without duplicate roots or duplicate
//! exclude entries.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use oxibrain_connectors::documents_config::{DEFAULT_MAX_FILE_BYTES, DocumentsConfig, RootEntry};

/// Summary of what `provision_space_vault` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionReport {
    pub vault_dir: PathBuf,
    /// `true` only when a fresh root entry was appended to `documents.toml`.
    pub root_added: bool,
    /// Exclude patterns appended to a legacy flat-vault root during this run.
    pub parent_excludes_added: Vec<String>,
}

/// Default include globs — mirror `documents_config::default_include`
/// verbatim so the saved `documents.toml` round-trips identically.
fn default_include() -> Vec<String> {
    vec![
        "**/*.md".to_string(),
        "**/*.txt".to_string(),
        "**/*.html".to_string(),
    ]
}

/// Default exclude globs — mirror `documents_config::default_exclude`.
fn default_exclude() -> Vec<String> {
    vec![
        "**/.git/**".to_string(),
        "**/.DS_Store".to_string(),
        "**/*.tmp".to_string(),
        "**/*.lock".to_string(),
    ]
}

/// Absolute on-disk path used in `documents.toml` for the per-space vault.
/// Absolute form (consistent with `init.rs`'s seed path) keeps round-trips
/// deterministic — `DocumentsConfig::load`'s tilde-expansion is a no-op for
/// entries that aren't tilde-prefixed. Shared with `space remove`, whose
/// scaffold detection is path-conditioned (spec §4.5).
pub(crate) fn root_path_for(home: &Path, name: &str) -> PathBuf {
    home.join(".oxi").join("spaces").join(name).join("vault")
}

/// Legacy flat `~/.oxi/vault` expanded against the calling process's `$HOME`. When
/// `DocumentsConfig::load` rewrites a tilde-prefixed flat-vault root, the
/// in-memory `path` is exactly this value.
fn expand_user_flat() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".oxi").join("vault")
    } else {
        PathBuf::from("~/.oxi/vault")
    }
}

/// `true` if `root_path` refers to the flat `<home>/.oxi/vault` directory,
/// in any of the spellings operators actually use (absolute, canonicalized,
/// tilde-prefixed with or without trailing slash, or post-tilde-expansion).
fn is_flat_root(root_path: &Path, flat: &Path, flat_canonical: &Path) -> bool {
    let candidates: [PathBuf; 5] = [
        flat.to_path_buf(),
        flat_canonical.to_path_buf(),
        PathBuf::from("~/.oxi/vault"),
        PathBuf::from("~/.oxi/vault/"),
        expand_user_flat(),
    ];
    candidates.iter().any(|c| c.as_path() == root_path)
}

/// Ensure `<home>/.oxi/spaces/<name>/vault/` exists, add a root entry that maps the
/// new vault into `<name>`, and (if a legacy flat `<home>/.oxi/vault/` root
/// exists in `documents.toml`) exclude `<name>/**` from it exactly once.
///
/// Idempotent:
/// - re-running with the same `name` does NOT append a duplicate root;
/// - re-running does NOT append a duplicate exclude to a flat-vault root;
/// - the vault directory is created with `create_dir_all`, so a pre-existing
///   directory is left intact.
pub fn provision_space_vault(brain_dir: &Path, home: &Path, name: &str) -> Result<ProvisionReport> {
    let vault_dir = home.join(".oxi").join("spaces").join(name).join("vault");
    std::fs::create_dir_all(&vault_dir)
        .with_context(|| format!("create vault dir {}", vault_dir.display()))?;

    let mut cfg = DocumentsConfig::load(brain_dir)?;
    let expected_path = root_path_for(home, name);

    let mut root_added = false;
    if let Some(existing) = cfg.roots.iter().find(|r| r.alias == name) {
        if existing.path != expected_path || existing.space != name {
            anyhow::bail!(
                "root alias '{name}' already exists with path '{}' and space '{}'",
                existing.path.display(),
                existing.space
            );
        }
    } else {
        cfg.roots.push(RootEntry {
            alias: name.to_string(),
            path: expected_path.clone(),
            space: name.to_string(),
            include: default_include(),
            exclude: default_exclude(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        });
        root_added = true;
    }

    // Parent fixup: a legacy flat root at `<home>/.oxi/vault` would
    // double-index this space's documents into its own space. Exclude the
    // subdir exactly once. Operators may have authored the flat root with a
    // tilde-prefixed or trailing-slash spelling; match all of them.
    let flat = home.join(".oxi").join("vault");
    let flat_canonical = std::fs::canonicalize(&flat).unwrap_or_else(|_| flat.clone());
    let mut parent_excludes_added = Vec::new();
    for root in cfg.roots.iter_mut() {
        if is_flat_root(&root.path, &flat, &flat_canonical) {
            let pat = format!("{name}/**");
            if !root.exclude.iter().any(|p| p == &pat) {
                root.exclude.push(pat.clone());
                parent_excludes_added.push(pat);
            }
        }
    }

    DocumentsConfig::save(brain_dir, &cfg)
        .with_context(|| format!("save documents.toml under {}", brain_dir.display()))?;

    Ok(ProvisionReport {
        vault_dir,
        root_added,
        parent_excludes_added,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seed a root entry directly into a freshly-loaded config and save it.
    /// Uses absolute paths so `DocumentsConfig::load`'s tilde-expansion is a
    /// no-op and round-trips are deterministic.
    fn seed_root(brain_dir: &Path, alias: &str, path: &Path, space: &str) {
        let mut cfg = DocumentsConfig::load(brain_dir).unwrap();
        cfg.roots.push(RootEntry {
            alias: alias.into(),
            path: path.to_path_buf(),
            space: space.into(),
            include: default_include(),
            exclude: default_exclude(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        });
        DocumentsConfig::save(brain_dir, &cfg).unwrap();
    }

    #[test]
    fn provisions_dir_root_and_idempotent() {
        let brain = tempfile::tempdir().unwrap();
        let h = tempfile::tempdir().unwrap();

        let r1 = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(r1.vault_dir.is_dir());
        assert_eq!(r1.vault_dir, h.path().join(".oxi/spaces/dev/vault"));
        assert!(r1.root_added);
        assert!(r1.parent_excludes_added.is_empty());

        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        assert_eq!(cfg.roots.iter().filter(|r| r.alias == "dev").count(), 1);
        let dev = cfg.roots.iter().find(|r| r.alias == "dev").unwrap();
        assert_eq!(dev.path, h.path().join(".oxi/spaces/dev/vault"));
        assert_eq!(dev.space, "dev");
        assert_eq!(dev.include, default_include());
        assert_eq!(dev.exclude, default_exclude());
        assert_eq!(dev.max_file_bytes, DEFAULT_MAX_FILE_BYTES);

        // Idempotent re-run: same name → no new root, no new exclude.
        let r2 = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(!r2.root_added);
        assert!(r2.parent_excludes_added.is_empty());
        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        assert_eq!(cfg.roots.iter().filter(|r| r.alias == "dev").count(), 1);
    }

    #[test]
    fn flat_vault_root_gains_exclude() {
        let brain = tempfile::tempdir().unwrap();
        let h = tempfile::tempdir().unwrap();
        // Seed a legacy flat vault root pointing at the absolute form so the
        // load round-trip is a no-op.
        let flat = h.path().join(".oxi").join("vault");
        seed_root(brain.path(), "vault", &flat, "personal");

        let r = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(r.root_added);
        assert_eq!(r.parent_excludes_added, vec!["dev/**".to_string()]);

        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        let flat = cfg
            .roots
            .iter()
            .find(|r0| r0.alias == "vault")
            .expect("flat vault root still present");
        assert!(flat.exclude.contains(&"dev/**".to_string()));

        // Re-run: exclude not duplicated.
        let r2 = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(r2.parent_excludes_added.is_empty());
        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        let flat = cfg.roots.iter().find(|r0| r0.alias == "vault").unwrap();
        let count = flat.exclude.iter().filter(|p| *p == "dev/**").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn flat_vault_root_with_tilde_prefix_also_matches() {
        let brain = tempfile::tempdir().unwrap();
        let h = tempfile::tempdir().unwrap();
        // Operator-authored documents.toml may still use tilde spelling;
        // `DocumentsConfig::load` will expand it against `$HOME`, so the
        // in-memory path matches our `expand_user_flat()` candidate.
        seed_root(
            brain.path(),
            "vault",
            &PathBuf::from("~/.oxi/vault"),
            "personal",
        );

        let r = provision_space_vault(brain.path(), h.path(), "work").unwrap();
        assert_eq!(r.parent_excludes_added, vec!["work/**".to_string()]);
    }

    #[test]
    fn flat_vault_root_with_trailing_slash_also_matches() {
        let brain = tempfile::tempdir().unwrap();
        let h = tempfile::tempdir().unwrap();
        seed_root(
            brain.path(),
            "vault",
            &PathBuf::from("~/.oxi/vault/"),
            "personal",
        );

        let r = provision_space_vault(brain.path(), h.path(), "work").unwrap();
        assert_eq!(r.parent_excludes_added, vec!["work/**".to_string()]);
    }

    #[test]
    fn alias_collision_with_different_path_errors() {
        let brain = tempfile::tempdir().unwrap();
        let h = tempfile::tempdir().unwrap();
        seed_root(brain.path(), "dev", &PathBuf::from("/elsewhere/dev"), "dev");

        let err = provision_space_vault(brain.path(), h.path(), "dev")
            .expect_err("alias collision must error");
        let msg = format!("{err}");
        assert!(msg.contains("alias 'dev'"), "msg = {msg}");
        assert!(msg.contains("/elsewhere/dev"), "msg = {msg}");
    }

    #[test]
    fn missing_brain_dir_is_created_on_save() {
        let parent = tempfile::tempdir().unwrap();
        let brain = parent.path().join("does-not-exist");
        let h = tempfile::tempdir().unwrap();

        let r = provision_space_vault(&brain, h.path(), "dev").unwrap();
        assert!(r.root_added);
        assert!(brain.join("documents.toml").is_file());
    }

    #[test]
    fn provisions_multiple_spaces_independently() {
        let brain = tempfile::tempdir().unwrap();
        let h = tempfile::tempdir().unwrap();

        for name in ["dev", "work", "personal"] {
            let r = provision_space_vault(brain.path(), h.path(), name).unwrap();
            assert!(r.root_added);
            assert!(r.vault_dir.is_dir());
        }

        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        let aliases: Vec<&str> = cfg.roots.iter().map(|r| r.alias.as_str()).collect();
        assert_eq!(aliases, vec!["dev", "work", "personal"]);
    }
}
