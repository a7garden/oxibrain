//! `oxibrain sync <DIR> [--space s]` — vault sync.
//!
//! Scans DIR recursively for `.md` files (oxibrain-connectors), classifies each
//! against the ledger's live note episodes for the space
//! (`oxibrain_core::classify_sync`), and ingests new/modified files with
//! `occurred_at` = file mtime so episode ids are stable across re-syncs.
//! Unchanged files are skipped — re-syncing an unchanged tree is a no-op.
//!
//! Modified paths append a new episode; the previous episode remains
//! (append-only ledger, P1). Its assertions stay live until retracted — check
//! `oxibrain contradictions` after syncing edits.

use anyhow::{Context, bail};
use oxibrain::{Brain, BrainConfig};
use oxibrain_connectors::scan_directory;
use oxibrain_core::{SyncAction, SyncFile, classify_sync, content_hash};
use oxibrain_ports::Timestamp;
use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Per-run outcome, returned for programmatic use and printed by the CLI.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub new: Vec<String>,
    pub unchanged: Vec<String>,
    pub modified: Vec<String>,
}

pub async fn run(dir: &Path, root: &Path, space: &str) -> anyhow::Result<()> {
    let report = sync(dir, root, space).await?;
    print_report(&report);
    Ok(())
}

/// Scan, classify, ingest. The store path convention is the file's path
/// relative to the sync root (forward slashes), so syncs are stable across
/// working directories and machines.
pub async fn sync(dir: &Path, root: &Path, space: &str) -> anyhow::Result<SyncReport> {
    if !root.is_dir() {
        bail!("not a directory: {}", root.display());
    }
    let files = scan_directory(root);
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let known = brain.note_hashes(&space_id).await?;

    // Content is dropped after hashing; keep it per path for the ingest pass.
    let mut contents: HashMap<String, (String, Timestamp)> = HashMap::new();
    let sync_files: Vec<SyncFile> = files
        .into_iter()
        .filter_map(|f| {
            let path = f.path.to_str()?.to_string();
            let modified = systemtime_to_timestamp(f.modified);
            let hash = content_hash(&f.content);
            contents.insert(path.clone(), (f.content, modified));
            Some(SyncFile {
                path,
                content_hash: hash,
                modified,
            })
        })
        .collect();

    let mut report = SyncReport::default();
    for action in classify_sync(sync_files, &known) {
        match action {
            SyncAction::New(f) => {
                ingest_one(&brain, &space_id, &contents, &f).await?;
                report.new.push(f.path);
            }
            SyncAction::Modified(f) => {
                ingest_one(&brain, &space_id, &contents, &f).await?;
                report.modified.push(f.path);
            }
            SyncAction::Unchanged(p) => report.unchanged.push(p),
        }
    }
    Ok(report)
}

async fn ingest_one(
    brain: &Brain,
    space_id: &str,
    contents: &HashMap<String, (String, Timestamp)>,
    f: &SyncFile,
) -> anyhow::Result<()> {
    let (content, occurred_at) = contents
        .get(&f.path)
        .with_context(|| format!("content missing for scanned path {}", f.path))?;
    brain
        .ingest_note(space_id, &f.path, content.clone(), *occurred_at)
        .await?;
    Ok(())
}

fn systemtime_to_timestamp(t: std::time::SystemTime) -> Timestamp {
    let millis = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp(millis)
}

fn print_report(report: &SyncReport) {
    println!(
        "sync complete: {} new, {} unchanged, {} modified",
        report.new.len(),
        report.unchanged.len(),
        report.modified.len()
    );
    for p in &report.new {
        println!("  new:       {p}");
    }
    for p in &report.modified {
        println!("  modified:  {p}");
    }
    if !report.modified.is_empty() {
        println!(
            "  note: modified paths append a new episode; previous versions remain — \
             check `oxibrain contradictions` and `retract` stale claims"
        );
    }
}
