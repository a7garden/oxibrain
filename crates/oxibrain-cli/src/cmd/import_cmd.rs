use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, file: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(file)?;
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    brain.import_jsonl(content).await?;
    println!("imported from {}", file.display());
    Ok(())
}
