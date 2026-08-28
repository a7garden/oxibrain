use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::{ClockPort, SystemClock};
use std::io::Read;
use std::path::Path;

pub async fn run(dir: &Path, path: std::path::PathBuf, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = crate::cmd::space_id(&brain, space).await?;
    let content = if path.as_path() == Path::new("-") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(&path)?
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ingest_unknown_space_creates_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path()))
            .await
            .unwrap();
        let _ = brain.ensure_space("personal").await.unwrap();
        drop(brain);
        let err = run(dir.path(), "-".into(), "nosuch").await.unwrap_err();
        assert!(err.to_string().contains("space 'nosuch' not found"));
        let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path()))
            .await
            .unwrap();
        let spaces = brain.list_spaces().await.unwrap();
        assert!(spaces.iter().all(|s| s.name != "nosuch"));
    }
}
