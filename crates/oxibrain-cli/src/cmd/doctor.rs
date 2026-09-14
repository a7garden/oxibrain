//! `oxibrain doctor` — store health check.
//!
//! Beyond the basic open + count, the daemonless surfaces: documents-plane
//! inventory and reconcile freshness, the memory-plane extraction backlog,
//! dangling `doc://` refs whose alias left `documents.toml`, and legacy
//! pull-mode sources (provenance-only after the two-plane migration — shown
//! so operators can move still-relevant paths into `documents.toml`, never
//! edited automatically).

use oxibrain::document_plane::format_diagnostic_lines;
use oxibrain::{Brain, BrainConfig, IndexOptions};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    // Unified-home layout: active root, owned subtree, and legacy state
    // (the operator's view of what migration would do — P1 diagnostics).
    let oxi = oxibrain::paths::oxi_home();
    let override_note = if std::env::var_os("OXI_HOME").is_some() {
        " (OXI_HOME)"
    } else {
        ""
    };
    println!("oxi home: {}{override_note}", oxi.display());
    println!("store dir: {}", dir.display());
    let models = oxibrain::migrate::models_paths(&oxi);
    let plan = oxibrain::migrate::preflight(&models);
    match plan.state {
        oxibrain::migrate::PlanState::NothingToDo => println!("legacy models: none"),
        oxibrain::migrate::PlanState::Ready => println!(
            "legacy models: {} file(s), {} byte(s) pending — run `oxibrain admin migrate --dry-run`",
            plan.files_to_copy, plan.bytes_to_copy
        ),
        oxibrain::migrate::PlanState::AlreadyMigrated => println!(
            "legacy models: already migrated (backup at {})",
            models.source.display()
        ),
        oxibrain::migrate::PlanState::Conflict => println!(
            "legacy models: CONFLICT between {} and {} — resolve by hand",
            models.source.display(),
            models.destination.display()
        ),
    }
    if let Some(status) = oxibrain::migrate::journal_status(&models.journal) {
        println!("migration journal: {status}");
    }

    let brain = Brain::open(BrainConfig::at(dir)).await?;
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
    // PDC diagnostics (pdc-adoption-v2 "Diagnostics"): grouped by code and
    // capped — doctor is a health surface, not a full audit log.
    if freshness.legacy_html > 0 {
        println!("  legacy html documents: {}", freshness.legacy_html);
    }
    if freshness.legacy_markdown > 0 {
        println!("  legacy markdown documents: {}", freshness.legacy_markdown);
    }
    if freshness.legacy_document_version > 0 {
        println!(
            "  legacy document-version (pdc-document/1) documents: {}",
            freshness.legacy_document_version
        );
    }
    if freshness.query_definitions > 0 {
        println!(
            "  pdc-query/1 query definitions (preserved, never executed): {}",
            freshness.query_definitions
        );
    }
    if freshness.diagnostics.is_empty() {
        println!("  pdc diagnostics: none");
    } else {
        println!("  pdc diagnostics: {}", freshness.diagnostics.len());
        for line in format_diagnostic_lines(&freshness.diagnostics, 50) {
            println!("  {line}");
        }
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

    // Orphaned source-registry rows: registered but never bound to an
    // episode (the tempdir-path leak; v13 deletes them at migration).
    let orphans = brain.orphan_source_count().await?;
    println!("orphan sources (never produced an episode): {orphans}");

    Ok(())
}
