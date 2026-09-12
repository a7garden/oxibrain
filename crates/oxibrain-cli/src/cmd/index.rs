//! `oxibrain index` — reconcile the documents cache (spec §7.3).
//!
//! Walks the configured roots from `documents.toml`, applies the pure
//! reconcile plan to `documents.db` in one atomic pass, and reports the
//! freshness outcome. `--embed` additionally embeds every missing chunk
//! (dense coverage); lexical reconciliation itself never calls a model.
//!
//! The `--documents` flag is the spec's canonical spelling (`index
//! --documents`); documents indexing is this command's one behavior with or
//! without it.

use oxibrain::{Brain, BrainConfig, IndexOptions};
use std::path::Path;

pub async fn run(dir: &Path, documents: bool, embed: bool) -> anyhow::Result<()> {
    let _ = documents; // accepted for the spec's canonical spelling; default on
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let brain = if embed {
        super::embed::require_local_embedder(brain)?
    } else {
        brain
    };
    let freshness = brain
        .index_documents(IndexOptions {
            embed,
            budget: None,
        })
        .await?;

    println!("reconciled roots: {}", freshness.reconciled_roots.len());
    if freshness.reconciled_roots.is_empty() {
        println!("  (no roots configured — add one to documents.toml)");
    }
    for (alias, reason) in &freshness.skipped_roots {
        println!("skipped root {alias}: {reason}");
    }
    if freshness.skipped_files > 0 {
        println!("skipped files: {}", freshness.skipped_files);
    }
    for locator in &freshness.stale_after_retry {
        println!("stale after retry: {locator}");
    }
    if embed {
        match freshness.dense_coverage {
            Some(cov) => println!("dense coverage: {:.1}%", cov * 100.0),
            None => println!("dense coverage: n/a (no chunks)"),
        }
    }
    Ok(())
}
