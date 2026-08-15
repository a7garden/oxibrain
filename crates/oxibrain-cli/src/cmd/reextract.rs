//! `oxibrain reextract` — re-extract all primary episodes with the configured
//! extractor (DESIGN §12.4).
//!
//! Uses a new ExtractorConfig (new model/prompt/major = new cache keys), so old
//! extraction cache entries are preserved (D8). Only uncached episodes are sent
//! to the LLM. Requires a configured LLM provider (see `cmd::llm`).

use crate::cmd::llm;
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::SystemClock;
use std::path::Path;
use std::sync::Arc;

pub async fn run(dir: &Path, space: &str) -> anyhow::Result<()> {
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
    );
    let summary = brain.reextract(&space_id, &config).await?;
    println!(
        "reextract: {} episodes done, {} failed, {} extracted, {} quarantined",
        summary.episodes_done, summary.episodes_failed, summary.extracted, summary.quarantined
    );
    for (episode_id, error) in &summary.failures {
        eprintln!("reextract: episode {episode_id} failed: {error}");
    }
    Ok(())
}
