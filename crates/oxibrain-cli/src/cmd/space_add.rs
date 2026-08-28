//! `oxibrain space add <name>` — explicit creation + provisioning.

use std::path::Path;

use anyhow::{Context, Result};
use oxibrain::{Brain, BrainConfig};

use crate::cmd::provision::provision_space_vault;

/// Create a space and, when the resolved brain dir is the default
/// `~/.oxi/brain`, provision its vault directory and documents root.
///
/// The "deliberate store" path (any of: `--dir` was passed, or `$HOME` is
/// missing, or the resolved dir is not the default brain dir) skips
/// provisioning — the operator is in charge of `documents.toml`.
pub async fn run(dir: &Path, explicit_dir: bool, home: Option<&Path>, raw: &str) -> Result<()> {
    let name =
        oxibrain_core::spaces::validate_space_name(raw).map_err(|e| anyhow::anyhow!("{e}"))?;

    let brain = Brain::open(BrainConfig::at(dir))
        .await
        .with_context(|| format!("open brain at {}", dir.display()))?;
    let id = brain.ensure_space(&name).await?;
    println!("space '{name}' -> {id}");

    match (home, explicit_dir) {
        (Some(h), false) => {
            let r = provision_space_vault(dir, h, &name)
                .with_context(|| format!("provision vault for '{name}'"))?;
            println!("vault dir: {}", r.vault_dir.display());
            if r.root_added {
                println!("documents root '{name}' added");
            }
            for pat in &r.parent_excludes_added {
                println!("parent flat root: excluded '{pat}'");
            }
        }
        _ => println!(
            "deliberate store (--dir): no vault provisioning — manage documents.toml roots yourself"
        ),
    }
    Ok(())
}
