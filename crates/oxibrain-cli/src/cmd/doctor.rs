//! `oxibrain doctor` — store health check.
//!
//! Beyond the basic open + count, the daemonless surfaces: documents-plane
//! inventory and reconcile freshness, the memory-plane extraction backlog,
//! dangling `doc://` refs whose alias left `documents.toml`, and legacy
//! pull-mode sources (provenance-only after the two-plane migration — shown
//! so operators can move still-relevant paths into `documents.toml`, never
//! edited automatically).

use oxibrain::{Brain, BrainConfig, IndexOptions};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    println!("ok: store at {}", dir.display());
    println!("episode count: {}", brain.episode_count().await?);

    // Documents plane: configured roots + cached files, then a reconcile
    // pass for current freshness (missing roots and skipped files surface
    // here — spec §7.3 "missing and actively changing files are surfaced").
    let (roots, files) = brain.document_counts().await?;
    println!("documents: {roots} configured root(s), {files} cached file(s)");
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await?;
    for (alias, reason) in &freshness.skipped_roots {
        println!("  skipped root {alias}: {reason}");
    }
    if freshness.skipped_files > 0 {
        println!("  skipped files: {}", freshness.skipped_files);
    }
    for locator in &freshness.stale_after_retry {
        println!("  stale after retry: {locator}");
    }

    // Memory-plane extraction backlog.
    let pending = brain.pending_extraction_stats().await?;
    match pending.oldest_seq {
        Some(seq) => println!(
            "pending extraction: {} episode(s), oldest seq {seq}",
            pending.count
        ),
        None => println!("pending extraction: none"),
    }

    // Dangling doc:// refs: episodes referencing an alias that is no longer
    // configured (provenance-only legacy rows, spec §11.3).
    let dangling = brain.dangling_document_refs().await?;
    if dangling.is_empty() {
        println!("dangling doc:// refs: none");
    } else {
        println!("dangling doc:// refs (alias missing from documents.toml):");
        for uri in dangling {
            println!("  {uri}");
        }
    }

    // Legacy pull sources: provenance-only. Default source listings hide
    // them; doctor is the one labeled surface that still shows them
    // (spec §11.1).
    let mut legacy = Vec::new();
    for space in brain.list_spaces().await? {
        for source in brain.list_sources(&space.id).await? {
            if source.mode == "pull" {
                legacy.push(format!("{} ({})", source.name, space.name));
            }
        }
    }
    if legacy.is_empty() {
        println!("legacy pull sources: none");
    } else {
        println!("legacy pull sources (provenance-only, no watcher):");
        for entry in legacy {
            println!("  {entry}");
        }
    }

    Ok(())
}
