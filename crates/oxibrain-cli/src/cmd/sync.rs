//! `oxibrain sync <DIR> [--space s]` — vault sync with occurrence identity.
//!
//! Scans DIR recursively for `.md`/`.html` files (oxibrain-connectors),
//! classifies each against the ledger's event-path state for the vault source
//! (`oxibrain_core::classify_event`), and ingests new/modified files via the
//! event path with derived occurrence IDs (§4.2).
//!
//! Occurrence chain: `occurrence_id = H(source_id, locator, predecessor, content_hash)`.
//! A → B → A creates three events because the predecessor differs.
//! Unchanged files are skipped — re-syncing an unchanged tree is a no-op.
//! Legacy episodes (pre-event-identity) participate in Unchanged classification
//! but are never re-ingested.

use anyhow::{Context, bail};
use oxibrain::{Brain, BrainConfig, IngestAttachment, SourceRef, TrustTier};
use oxibrain_connectors::scan_directory;
use oxibrain_core::{
    SyncAction, SyncFile, classify_event, content_hash, occurrence_id, sync::LocatorState,
};
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

/// Scan, classify, ingest via event path. The locator convention is the file's
/// path relative to the sync root (forward slashes).
pub async fn sync(dir: &Path, root: &Path, space: &str) -> anyhow::Result<SyncReport> {
    if !root.is_dir() {
        bail!("not a directory: {}", root.display());
    }
    let files = scan_directory(root);
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;

    // Register the vault as a pull source. Source name = canonical path.
    let source_name = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let source_id = brain
        .ensure_source(&space_id, &source_name, "document_revision", "pull")
        .await?;

    // Fetch both classification inputs.
    let legacy = brain.note_hashes(&space_id).await?;
    let event_states = brain.locator_states(&space_id, &source_id).await?;

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
    let now = brain.clock_now();
    for action in classify_event(sync_files, &legacy, &event_states) {
        match action {
            SyncAction::New(f) => {
                ingest_event_one(
                    &brain,
                    &space_id,
                    &source_id,
                    &contents,
                    &event_states,
                    &f,
                    now,
                )
                .await?;
                report.new.push(f.path);
            }
            SyncAction::Modified(f) => {
                ingest_event_one(
                    &brain,
                    &space_id,
                    &source_id,
                    &contents,
                    &event_states,
                    &f,
                    now,
                )
                .await?;
                report.modified.push(f.path);
            }
            SyncAction::Unchanged(p) => report.unchanged.push(p),
        }
    }
    Ok(report)
}

async fn ingest_event_one(
    brain: &Brain,
    space_id: &str,
    source_id: &str,
    contents: &HashMap<String, (String, Timestamp)>,
    event_states: &HashMap<String, LocatorState>,
    f: &SyncFile,
    now: Timestamp,
) -> anyhow::Result<()> {
    let (content, _occurred_at) = contents
        .get(&f.path)
        .with_context(|| format!("content missing for scanned path {}", f.path))?;

    // Derive occurrence: predecessor is the latest occurrence for this locator.
    let predecessor = event_states
        .get(&f.path)
        .map(|s| s.latest_occurrence_id.as_str());
    let occ = occurrence_id(source_id, &f.path, predecessor, &f.content_hash);

    let attachment = IngestAttachment {
        source_id: source_id.into(),
        occurrence_id: occ,
        accepted_at: now,
        principal: "sync".into(),
        claims_json: "{}".into(),
    };

    brain
        .ingest_event(
            space_id,
            content.clone(),
            SourceRef::Note {
                path: f.path.clone(),
            },
            TrustTier::Trusted,
            Some(&attachment),
            "vault-sync",
        )
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
    if !report.new.is_empty() {
        for p in &report.new {
            println!("  new: {p}");
        }
    }
    if !report.modified.is_empty() {
        for p in &report.modified {
            println!("  modified: {p}");
        }
    }
    println!(
        "sync complete: {} new, {} unchanged, {} modified",
        report.new.len(),
        report.unchanged.len(),
        report.modified.len()
    );
}
