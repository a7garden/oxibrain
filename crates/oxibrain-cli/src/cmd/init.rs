use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(space).await?;
    println!("initialized brain at {}", dir.display());
    println!("space '{space}' -> {id}");
    // ADR-005: init stays offline; say so instead of surprising the user later.
    println!(
        "model weights pull automatically on first extract — pre-fetch with `oxibrain model pull`"
    );
    Ok(())
}
