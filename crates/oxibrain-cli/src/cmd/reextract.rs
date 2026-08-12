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
    let (port, model, mechanism) = llm::from_env()?;
    let clock = Arc::new(SystemClock);
    let brain = Brain::with_llm(BrainConfig::at(dir), clock, port).await?;
    let space_id = brain.ensure_space(space).await?;
    let config = llm::config(model, mechanism);

    let summary = brain.reextract(&space_id, &config).await?;
    println!(
        "reextract: {} episodes done, {} failed, {} extracted, {} quarantined",
        summary.episodes_done, summary.episodes_failed, summary.extracted, summary.quarantined
    );
    Ok(())
}
