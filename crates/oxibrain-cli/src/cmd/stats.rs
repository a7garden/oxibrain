use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let episodes = brain.episode_count().await?;
    println!("dir:    {}", dir.display());
    println!("episodes: {episodes}");
    Ok(())
}
