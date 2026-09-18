//! `oxibrain extract --pending` — drain the memory-plane backlog.
//!
//! The queue-less extraction path (spec §9.5): the backlog is every primary,
//! non-document, non-redacted episode with no extraction row for the current
//! extractor. Each episode is extracted outside any DB transaction, validated
//! against the registry, and projected; failures leave the episode cached as
//! pending so a later run rediscovers it. Requires a configured LLM provider
//! (see `cmd::llm`).
//!
//! The drain summary separates episodes whose extraction was accepted
//! (cache row + projection) from those that only produced validator
//! rejections (`extraction_failures` rows), so a drain that yielded
//! nothing is visible as such.

use crate::cmd::llm;
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::{ClockPort, SystemClock};
use oxibrain_store::Store;
use oxibrain_store::extraction::extraction_run_counts;
use std::path::Path;
use std::sync::Arc;

pub async fn run(dir: &Path, limit: Option<usize>) -> anyhow::Result<()> {
    let provider = llm::from_env().await?;
    let clock = Arc::new(SystemClock);
    let brain = match provider.tokenizer.clone() {
        Some(tok) => {
            Brain::with_llm_and_tokenizer(
                BrainConfig::at(dir),
                clock.clone(),
                provider.port.clone(),
                tok,
            )
            .await?
        }
        None => Brain::with_llm(BrainConfig::at(dir), clock.clone(), provider.port.clone()).await?,
    }
    // §9.5: the drain must key the extraction cache by the resolved
    // provider's identity (model id + mechanism + weights digest), or a
    // model swap silently reuses extractions produced by the old weights.
    .with_extractor_config(llm::config(
        provider.model_id.clone(),
        provider.mechanism,
        provider.model_digest.clone(),
        provider.profile_id(),
    ));
    let before = brain.pending_extraction_stats().await?;
    if before.count == 0 {
        println!("pending extraction: none");
        return Ok(());
    }
    let limit = limit.unwrap_or(usize::MAX);
    let started_at = clock.now();
    let extracted = brain.extract_uncached(limit).await?;
    let ended_at = clock.now();
    let after = brain.pending_extraction_stats().await?;
    // Outcome counts for exactly this run's window. A second read-only
    // connection is safe next to the facade's writer (WAL, query-only);
    // the facade exposes no cache-row reader, and the summary is a CLI
    // presentation concern.
    let outcomes = {
        let conn = Store::open_read_only(dir)?;
        extraction_run_counts(&conn, started_at, ended_at)?
    };
    println!(
        "extracted {extracted} episode(s): {} accepted, {} rejected ({} invalid claims recorded); {} still pending (oldest seq {:?})",
        outcomes.accepted,
        outcomes.rejected_episodes,
        outcomes.failure_rows,
        after.count,
        after.oldest_seq
    );
    Ok(())
}
