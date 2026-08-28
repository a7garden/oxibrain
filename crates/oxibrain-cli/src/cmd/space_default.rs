//! `oxibrain space default [<name>]` — print or set the default space.

use crate::cmd::space_id;
use oxibrain::config::UserConfig;
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, home: Option<&Path>, name: Option<&str>) -> anyhow::Result<()> {
    let Some(home) = home else {
        anyhow::bail!("$HOME is required to read/write ~/.oxi/config.toml");
    };
    let Some(name) = name else {
        let cfg = UserConfig::load(Some(home)).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("{}", cfg.default_space);
        return Ok(());
    };
    let name =
        oxibrain_core::spaces::validate_space_name(name).map_err(|e| anyhow::anyhow!("{e}"))?;
    // The default must exist in the resolved brain (spec §4.2).
    let brain = Brain::open_ro(BrainConfig::at(dir)).await?;
    let _ = space_id(&brain, &name).await?;
    UserConfig::set_default_space(home, &name).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("default space: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_and_print_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let _ = b.ensure_space("dev").await.unwrap();
        drop(b);
        run(dir.path(), Some(home.path()), Some("dev"))
            .await
            .unwrap();
        assert!(UserConfig::load(Some(home.path())).unwrap().default_space == "dev");
        run(dir.path(), Some(home.path()), None).await.unwrap(); // prints "dev"
    }

    #[tokio::test]
    async fn set_unknown_space_fails_without_writing() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        assert!(
            run(dir.path(), Some(home.path()), Some("ghost"))
                .await
                .is_err()
        );
        assert!(!UserConfig::config_path(home.path()).exists());
    }
}
