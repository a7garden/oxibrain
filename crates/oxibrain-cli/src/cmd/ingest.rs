use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::{ClockPort, SystemClock};
use std::io::Read;
use std::path::Path;

pub async fn run(dir: &Path, path: std::path::PathBuf, space: &str) -> anyhow::Result<()> {
    let content = if path.as_path() == Path::new("-") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(&path)?
    };
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let id = brain
        .ingest_note(
            &space_id,
            &path.display().to_string(),
            content,
            SystemClock.now(),
        )
        .await?;
    println!("ingested episode {id}");
    Ok(())
}
