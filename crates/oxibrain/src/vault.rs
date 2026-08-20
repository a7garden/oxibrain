//! Vault pull-sync orchestration (ARCHITECTURE.md §4.2): scan a directory of
//! `.md`/`.html` notes, classify each file against the ledger's event-path
//! state, and ingest new/modified files with derived occurrence ids.
//!
//! One implementation, three hosts (P6): the `oxibrain sync` CLI, the
//! daemon's `sync/run` RPC, and the daemon's source watcher. The CLI path
//! opens its own store; the daemon paths run against the serving `Brain` —
//! the P8 single-writer lock makes the daemon the only process that can
//! host recurring ingestion while serving.
//!
//! Occurrence chain: `occurrence_id = H(source_id, locator, predecessor,
//! content_hash)` — A → B → A creates three events because the predecessor
//! differs. Unchanged files are skipped: re-syncing an unchanged tree is a
//! no-op. Legacy episodes (pre-event-identity) participate in Unchanged
//! classification but are never re-ingested.

use oxibrain_connectors::scan_directory;
use oxibrain_core::{
    SyncAction, SyncFile, classify_event, content_hash, occurrence_id, sync::LocatorState,
};
use oxibrain_ports::{BrainError, Timestamp};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::{Brain, IngestAttachment, SourceRef, TrustTier};

/// Per-pass outcome. Serialized as the `sync/run` RPC result.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncReport {
    pub new: Vec<String>,
    pub unchanged: Vec<String>,
    pub modified: Vec<String>,
}

/// A registered pull source (`kind = "document_revision"`, `mode = "pull"`)
/// the daemon can watch: the vault directory — source name is its canonical
/// path (§4.2) — and the space it feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullSource {
    pub space: String,
    pub dir: PathBuf,
}

/// Enumerate registered pull sources across all spaces. The daemon watches
/// each `dir` at startup and after a `sync/run` registration.
pub async fn pull_sources(brain: &Brain) -> Result<Vec<PullSource>, BrainError> {
    let mut out = Vec::new();
    for s in brain.list_spaces().await? {
        for src in brain.list_sources(&s.id).await? {
            if src.kind == "document_revision" && src.mode == "pull" {
                out.push(PullSource {
                    space: s.name.clone(),
                    dir: PathBuf::from(&src.name),
                });
            }
        }
    }
    Ok(out)
}

/// Scan, classify, ingest via the event path. The locator convention is the
/// file's path relative to the sync root (forward slashes).
pub async fn sync_vault(brain: &Brain, root: &Path, space: &str) -> Result<SyncReport, BrainError> {
    if !root.is_dir() {
        return Err(BrainError::Config(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    let files = scan_directory(root);
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
                    brain,
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
                    brain,
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
) -> Result<(), BrainError> {
    let (content, _occurred_at) = contents.get(&f.path).ok_or_else(|| {
        BrainError::Config(format!("content missing for scanned path {}", f.path))
    })?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pull_sources_lists_registered_vaults_with_space() {
        let dir = tempfile::tempdir().unwrap();
        let brain = Brain::open(crate::BrainConfig::at(dir.path()))
            .await
            .unwrap();
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("a.md"), "# a\n").unwrap();

        sync_vault(&brain, vault.path(), "personal").await.unwrap();

        let sources = pull_sources(&brain).await.unwrap();
        let canonical = vault
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            sources,
            vec![PullSource {
                space: "personal".into(),
                dir: PathBuf::from(canonical),
            }]
        );
    }
}
