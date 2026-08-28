//! `oxibrain init` — initialize a brain store (spec §4).
//!
//! The CLI arm has already resolved the space; init validates the name,
//! ensures the space exists, and — only when the resolved dir is a default
//! dir (no `--dir` passed and a home is known) — provisions the per-space
//! vault directory and its `documents.toml` root (§4.3), then writes
//! `~/.oxi/config.toml` with `default_space` when no config file exists
//! yet. An explicit `--dir` is a deliberate store: nothing under home is
//! touched. Provisioning is idempotent: re-running init neither clobbers
//! an existing config nor duplicates roots.

use crate::cmd::provision::provision_space_vault;
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(
    dir: &Path,
    space: &str,
    explicit_dir: bool,
    home: Option<&Path>,
) -> anyhow::Result<()> {
    let name =
        oxibrain_core::spaces::validate_space_name(space).map_err(|e| anyhow::anyhow!("{e}"))?;
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(&name).await?;
    println!("initialized brain at {}", dir.display());
    println!("space '{name}' -> {id}");

    if let (Some(h), false) = (home, explicit_dir) {
        let r = provision_space_vault(dir, h, &name)?;
        println!("vault dir: {}", r.vault_dir.display());
        if r.root_added {
            println!("documents root '{name}' added");
        }
        // v2.13 (ADR-013): init no longer seeds `~/.oxi/config.toml`
        // `default_space` — creation is not resolution, and resolution is
        // gone: every space-scoped call passes `space` explicitly.
    }

    // ADR-005: init stays offline; say so instead of surprising the user later.
    println!(
        "model weights pull automatically on first extract — pre-fetch with `oxibrain model pull`"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_provisions_per_space_vault() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        run(dir.path(), "personal", false, Some(home.path()))
            .await
            .unwrap();
        assert!(home.path().join(".oxi/vault/personal").is_dir());
        assert!(dir.path().join("documents.toml").exists());
        // v2.13: no config.toml is written — default_space is gone (ADR-013).
        assert!(!home.path().join(".oxi/config.toml").exists());
        // Idempotent: second init neither clobbers nor duplicates.
        run(dir.path(), "personal", false, Some(home.path()))
            .await
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join("documents.toml")).unwrap();
        assert_eq!(text.matches("[[root]]").count(), 1);
    }

    #[tokio::test]
    async fn explicit_dir_stays_off_home() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        run(dir.path(), "work", true, Some(home.path()))
            .await
            .unwrap();
        assert!(!dir.path().join("documents.toml").exists());
        assert!(!home.path().join(".oxi/config.toml").exists());
    }
}
