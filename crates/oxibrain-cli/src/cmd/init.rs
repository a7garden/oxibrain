use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(space).await?;
    println!("initialized brain at {}", dir.display());
    println!("space '{space}' -> {id}");
    Ok(())
}
