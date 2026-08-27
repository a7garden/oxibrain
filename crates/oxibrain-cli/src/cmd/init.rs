//! `oxibrain init` — initialize a brain store (spec §4).
//!
//! Optionally seeds `documents.toml` with a `vault` root when ALL of the
//! spec's conditions hold (§4.1 "explicit init, default dir"):
//!
//! 1. the user passed no `--dir` (an explicit dir is a deliberate store and
//!    is never touched beyond creating it), and
//! 2. the resolved dir IS the default `~/.oxi/brain`, and
//! 3. `~/.oxi/vault` exists (the Oxi Foundation vault layout).
//!
//! An existing `documents.toml` is never overwritten.

use oxibrain::{Brain, BrainConfig};
use oxibrain_connectors::documents_config::{DocumentsConfig, RootEntry};
use std::path::Path;

pub async fn run(
    dir: &Path,
    space: &str,
    explicit_dir: bool,
    home: Option<&Path>,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(space).await?;
    println!("initialized brain at {}", dir.display());
    println!("space '{space}' -> {id}");
    if let Some(seed_path) = seed_target(dir, explicit_dir, home) {
        let cfg = DocumentsConfig {
            roots: vec![RootEntry {
                alias: "vault".into(),
                path: seed_path.clone(),
                space: "personal".into(),
                include: vec![
                    "**/*.md".into(),
                    "**/*.txt".into(),
                    "**/*.html".into(),
                ],
                exclude: vec![
                    "**/.git/**".into(),
                    "**/.DS_Store".into(),
                    "**/*.tmp".into(),
                    "**/*.lock".into(),
                ],
                max_file_bytes: oxibrain_connectors::documents_config::DEFAULT_MAX_FILE_BYTES,
            }],
        };
        cfg.validate().map_err(|e| anyhow::anyhow!("seed: {e}"))?;
        let seeded = seed_path.display();
        DocumentsConfig::save(dir, &cfg).map_err(|e| anyhow::anyhow!("seed: {e}"))?;
        println!("seeded documents.toml with vault root ({seeded})");
    }

    // ADR-005: init stays offline; say so instead of surprising the user later.
    println!(
        "model weights pull automatically on first extract — pre-fetch with `oxibrain model pull`"
    );
    Ok(())
}

/// The vault path to seed, when the spec §4 conditions hold. `None` means
/// "do not seed".
fn seed_target(dir: &Path, explicit_dir: bool, home: Option<&Path>) -> Option<std::path::PathBuf> {
    if explicit_dir {
        return None; // --dir passed: never seed.
    }
    let home = home?;
    if dir != home.join(".oxi").join("brain") {
        return None; // resolved dir is not the default brain dir.
    }
    let vault = home.join(".oxi").join("vault");
    if !vault.is_dir() {
        return None; // no foundation vault: nothing to point at.
    }
    if dir.join("documents.toml").exists() {
        return None; // never overwrite an existing config.
    }
    Some(vault)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_target_requires_default_dir_and_vault() {
        let home = tempfile::TempDir::new().unwrap();
        let brain_dir = home.path().join(".oxi").join("brain");
        std::fs::create_dir_all(&brain_dir).unwrap();

        // No vault yet: no seed.
        assert_eq!(seed_target(&brain_dir, false, Some(home.path())), None);
        // --dir passed: never seed.
        assert_eq!(seed_target(&brain_dir, true, Some(home.path())), None);
        // Different dir: no seed.
        let other = home.path().join("elsewhere");
        assert_eq!(seed_target(&other, false, Some(home.path())), None);

        // Vault exists + default dir + no explicit --dir: seed.
        std::fs::create_dir_all(home.path().join(".oxi").join("vault")).unwrap();
        assert_eq!(
            seed_target(&brain_dir, false, Some(home.path())),
            Some(home.path().join(".oxi").join("vault"))
        );

        // Existing documents.toml: untouched.
        std::fs::write(brain_dir.join("documents.toml"), "[[root]]\n").unwrap();
        assert_eq!(seed_target(&brain_dir, false, Some(home.path())), None);
    }
}
