//! `oxibrain extract <episode-id>` — synchronous single-episode extraction.
//!
//! Reads the episode, calls the LLM, validates claims against the registry,
//! quarantines invalid output, and projects valid assertions. Realtime mode
//! (no job queue). Requires a configured LLM provider (see `cmd::llm`).

use crate::cmd::llm;
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::SystemClock;
use std::path::Path;
use std::sync::Arc;

pub async fn run(dir: &Path, episode_id: &str, space: &str) -> anyhow::Result<()> {
    let (port, model, mechanism) = llm::from_env()?;
    let clock = Arc::new(SystemClock);
    let brain = Brain::with_llm(BrainConfig::at(dir), clock, port).await?;
    let space_id = brain.ensure_space(space).await?;
    let config = llm::config(model, mechanism);

    let summary = brain.extract_one(&space_id, episode_id, &config).await?;
    println!(
        "episode {episode_id}: {} extracted, {} quarantined (repair attempts consumed)",
        summary.extracted, summary.quarantined
    );
    Ok(())
}
