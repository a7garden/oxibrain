use oxibrain::{Brain, BrainConfig};
use std::path::{Path, PathBuf};

pub async fn run(dir: &Path, out: Option<PathBuf>) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let jsonl = brain.export_jsonl().await?;
    match out {
        Some(path) => {
            std::fs::write(&path, jsonl.as_bytes())?;
            println!("wrote {} bytes to {}", jsonl.len(), path.display());
        }
        None => {
            println!("{jsonl}");
        }
    }
    Ok(())
}
