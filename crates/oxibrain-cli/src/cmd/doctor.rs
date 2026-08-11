use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    println!("ok: store at {}", dir.display());
    println!("episode count: {}", brain.episode_count().await?);
    // M0 doctor: open + count. Orphan/index/belief checks land with those subsystems (M1+).
    Ok(())
}
