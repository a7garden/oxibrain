//! `oxibrain extract --pending` — drain the memory-plane backlog.
//!
//! The queue-less extraction path (spec §9.5): the backlog is every primary,
//! non-document, non-redacted episode with no extraction row for the current
//! extractor. Each episode is extracted outside any DB transaction, validated
//! against the registry, and projected; failures leave the episode cached as
//! pending so a later run rediscovers it. Requires a configured LLM provider
//! (see `cmd::llm`).

use crate::cmd::llm;
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::SystemClock;
use std::path::Path;
use std::sync::Arc;

pub async fn run(dir: &Path, limit: Option<usize>) -> anyhow::Result<()> {
    let provider = llm::from_env().await?;
    let clock = Arc::new(SystemClock);
    let brain = match provider.tokenizer.clone() {
        Some(tok) => {
            Brain::with_llm_and_tokenizer(BrainConfig::at(dir), clock, provider.port.clone(), tok)
                .await?
        }
        None => Brain::with_llm(BrainConfig::at(dir), clock, provider.port.clone()).await?,
    };
    let before = brain.pending_extraction_stats().await?;
    if before.count == 0 {
        println!("pending extraction: none");
        return Ok(());
    }
    let limit = limit.unwrap_or(usize::MAX);
    let extracted = brain.extract_uncached(limit).await?;
    let after = brain.pending_extraction_stats().await?;
    println!(
        "extracted {extracted} episode(s); {} still pending (oldest seq {:?})",
        after.count, after.oldest_seq
    );
    Ok(())
}
