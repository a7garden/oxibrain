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
    let provider = llm::from_env().await?;
    let clock = Arc::new(SystemClock);
    let brain = match provider.tokenizer.clone() {
        Some(tok) => {
            Brain::with_llm_and_tokenizer(BrainConfig::at(dir), clock, provider.port.clone(), tok)
                .await?
        }
        None => Brain::with_llm(BrainConfig::at(dir), clock, provider.port.clone()).await?,
    };
    let space_id = brain.ensure_space(space).await?;
    let config = llm::config(
        provider.model_id.clone(),
        provider.mechanism,
        provider.model_digest.clone(),
        provider.profile_id(),
    );

    let summary = brain.extract_one(&space_id, episode_id, &config).await?;
    println!(
        "episode {episode_id}: {} extracted, {} quarantined (repair attempts consumed)",
        summary.extracted, summary.quarantined
    );
    Ok(())
}
