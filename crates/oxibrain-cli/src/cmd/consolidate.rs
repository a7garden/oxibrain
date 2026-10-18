//! `oxibrain admin consolidate` — cluster episodes into Derived summaries.
//!
//! `--mode topic` (default): shared-statement clusters → thematic summaries
//! (§13). `--mode task`: Task/Code/Artifact-anchored clusters → runbook
//! episodes (experience memory spec §4, M11). Requires a configured LLM
//! provider (see `cmd::llm`); the model call runs outside any store
//! transaction by construction.

use crate::cmd::llm;
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::SystemClock;
use std::path::Path;
use std::sync::Arc;

pub async fn run(dir: &Path, space: &str, mode: &str, min_members: usize) -> anyhow::Result<()> {
    let provider = llm::from_env().await?;
    let clock = Arc::new(SystemClock);
    let brain = match provider.tokenizer.clone() {
        Some(tok) => {
            Brain::with_llm_and_tokenizer(BrainConfig::at(dir), clock, provider.port.clone(), tok)
                .await?
        }
        None => Brain::with_llm(BrainConfig::at(dir), clock, provider.port.clone()).await?,
    };
    let space_id = crate::cmd::space_id(&brain, space).await?;
    let config = llm::config(
        provider.model_id.clone(),
        provider.mechanism,
        provider.model_digest.clone(),
        provider.profile_id(),
    );
    match mode {
        "task" => {
            let ids = brain
                .consolidate_runbooks(&space_id, &config, min_members)
                .await?;
            println!(
                "consolidate: {} runbook(s) written (mode=task, min_members={min_members})",
                ids.len()
            );
            for id in &ids {
                println!("  runbook episode: {id}");
            }
        }
        "topic" => {
            let ids = brain.consolidate(&space_id, &config).await?;
            println!(
                "consolidate: {} summary episode(s) written (mode=topic)",
                ids.len()
            );
            for id in &ids {
                println!("  summary episode: {id}");
            }
        }
        other => anyhow::bail!("unknown mode '{other}' — expected topic or task"),
    }
    Ok(())
}
