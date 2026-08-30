//! Journaled, resumable home-layout migrations (unified Oxi home).
//!
//! The compatibility release moves legacy layouts into the canonical
//! `OXI_HOME` tree without ever destroying data. The engine here is shared
//! by every oxibrain migration (currently `models`, more may follow) and
//! obeys the cross-app migration contract:
//!
//! - **Preflight** produces a dry-run plan: source, destination, file
//!   count, byte total, and a [`PlanState`] (nothing to do / ready /
//!   already migrated / conflict). Dry-run callers stop here.
//! - **Conflict refusal**: when the destination holds a file that differs
//!   from its source counterpart (or a file the source does not have), the
//!   migration aborts reporting both paths and touches nothing. Partial
//!   copies (destination missing some files) are *not* conflicts — they are
//!   the resumable case.
//! - **Journal before mutation**: the journal is written (atomically,
//!   temp + rename) before the first copy, and flipped to `complete` only
//!   after the verify pass. Every step is idempotent, so re-running the
//!   migration after a crash resumes cleanly.
//! - **Copy + verify, never rename**: files are copied to `<dest>.part`
//!   siblings, fsynced, then renamed into place; the verify pass re-hashes
//!   both trees. The source is never deleted or modified — it stays as the
//!   recoverable backup for the whole compatibility window (rename-based
//!   moves are deferred to the cutover release).
//!
//! Pure decision helpers take injected paths; nothing here reads env vars,
//! so tests stay hermetic.

use std::fs;
use std::path::{Path, PathBuf};

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Resolved paths for the models migration:
/// `<oxi-home>/models` → `<oxi-home>/brain/models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPaths {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub journal: PathBuf,
}

/// Models layout for an Oxi home: legacy `<home>/models` into the
/// brain-owned `<home>/brain/models`, journal beside the destination.
pub fn models_paths(home: &Path) -> MigrationPaths {
    MigrationPaths {
        source: home.join("models"),
        destination: home.join("brain").join("models"),
        journal: home.join("brain").join(".models.migration-journal.json"),
    }
}

/// Outcome of a preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    /// Nothing to migrate (no source, or source and destination both absent).
    NothingToDo,
    /// Source exists and can be copied into the destination.
    Ready,
    /// The destination already mirrors the source (or the source is gone
    /// and the destination is populated): migration is a no-op.
    AlreadyMigrated,
    /// Old and new locations both exist with differing content; the
    /// operator must resolve this by hand. Never touched automatically.
    Conflict,
}

/// Dry-run plan: what a migration would do, without touching anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationPlan {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub state: PlanState,
    /// Files the copy step would write (0 unless `Ready`).
    pub files_to_copy: u64,
    /// Bytes the copy step would write (0 unless `Ready`).
    pub bytes_to_copy: u64,
}

/// Errors surfaced by the migration engine.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Old and new locations disagree — both paths are reported and both
    /// are left untouched.
    #[error("conflicting old/new locations — resolve by hand:\n  old: {old}\n  new: {new}")]
    Conflict { old: PathBuf, new: PathBuf },
    /// The verify pass found the destination does not mirror the source.
    #[error("verification failed after copy: {0}")]
    Verify(String),
    #[error("io error: {0}")]
    Io(String),
}

/// One relative file entry inside a tree: relative path + content digest.
type FileDigest = (PathBuf, String);

/// Preflight: classify the old/new pair and (when `Ready`) count the work.
/// Reads only — safe to call for `--dry-run`.
pub fn preflight(paths: &MigrationPaths) -> MigrationPlan {
    let state = classify(paths);
    let (files_to_copy, bytes_to_copy) = if state == PlanState::Ready {
        pending_copy_work(paths)
    } else {
        (0, 0)
    };
    MigrationPlan {
        source: paths.source.clone(),
        destination: paths.destination.clone(),
        state,
        files_to_copy,
        bytes_to_copy,
    }
}

/// Classify the migration without counting work.
fn classify(paths: &MigrationPaths) -> PlanState {
    if !paths.source.is_dir() {
        // Source gone: a populated destination means the move already
        // happened; neither side existing is a fresh install.
        return if paths.destination.is_dir() {
            PlanState::AlreadyMigrated
        } else {
            PlanState::NothingToDo
        };
    }
    let source_files = walk_tree(&paths.source).unwrap_or_default();
    if source_files.is_empty() {
        // Nothing to move; destination (if any) is authoritative.
        return PlanState::NothingToDo;
    }
    let dest_files = match walk_tree(&paths.destination) {
        Ok(files) => files,
        Err(_) => return PlanState::Ready, // destination absent → fresh copy
    };
    let dest_map: std::collections::BTreeMap<&Path, &str> = dest_files
        .iter()
        .map(|(p, d)| (p.as_path(), d.as_str()))
        .collect();
    // Same relative path with different bytes — a real old/new conflict.
    let differing = source_files
        .iter()
        .any(|(p, d)| dest_map.get(p.as_path()).is_some_and(|e| *e != d.as_str()));
    // Destination files the source does not have are unexpected in a
    // copy-forward migration: surface them as a conflict too.
    let source_map: std::collections::BTreeSet<&Path> =
        source_files.iter().map(|(p, _)| p.as_path()).collect();
    let extra = dest_files
        .iter()
        .any(|(p, _)| !source_map.contains(p.as_path()));
    if differing || extra {
        return PlanState::Conflict;
    }
    // Everything already mirrored (or the destination is empty and nothing
    // was copied yet) decides Ready vs AlreadyMigrated; anything in between
    // is the resumable partial-copy case.
    let all_present = source_files
        .iter()
        .all(|(p, d)| dest_map.get(p.as_path()) == Some(&d.as_str()));
    if all_present {
        PlanState::AlreadyMigrated
    } else {
        PlanState::Ready
    }
}

/// Live journal status for diagnostics: `Some("in_progress"|"complete")`
/// when a journal exists, `None` otherwise.
pub fn journal_status(journal: &Path) -> Option<String> {
    let text = fs::read_to_string(journal).ok()?;
    serde_json::from_str::<MigrationJournal>(&text)
        .ok()
        .map(|j| j.status)
}

/// `(files, bytes)` the copy step would still write when `Ready`.
fn pending_copy_work(paths: &MigrationPaths) -> (u64, u64) {
    let source_files = match walk_tree(&paths.source) {
        Ok(files) => files,
        Err(_) => return (0, 0),
    };
    let mut files = 0u64;
    let mut bytes = 0u64;
    for (rel, digest) in &source_files {
        let dest_file = paths.destination.join(rel);
        if same_digest(&dest_file, digest) {
            continue;
        }
        if let Ok(meta) = fs::metadata(paths.source.join(rel)) {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// Run (or resume) the migration. Safe to call repeatedly: already-copied
/// files are skipped by digest, the journal is written before the first
/// copy, and the source is never modified.
pub fn migrate(paths: &MigrationPaths) -> Result<MigrationReport, MigrationError> {
    let plan = preflight(paths);
    match plan.state {
        PlanState::NothingToDo | PlanState::AlreadyMigrated => {
            return Ok(MigrationReport {
                state: plan.state,
                copied_files: 0,
                copied_bytes: 0,
            });
        }
        PlanState::Conflict => {
            return Err(MigrationError::Conflict {
                old: paths.source.clone(),
                new: paths.destination.clone(),
            });
        }
        PlanState::Ready => {}
    }

    // Journal BEFORE the first filesystem mutation (the first copy).
    let journal = MigrationJournal {
        version: 1,
        source: paths.source.display().to_string(),
        destination: paths.destination.display().to_string(),
        status: "in_progress".into(),
        started_at: unix_now(),
    };
    journal.write_atomic(&paths.journal)?;

    let copied = copy_pending(paths)?;
    verify(paths)?;

    let journal = MigrationJournal {
        status: "complete".into(),
        ..journal
    };
    journal.write_atomic(&paths.journal)?;
    Ok(MigrationReport {
        state: PlanState::Ready,
        copied_files: copied.0,
        copied_bytes: copied.1,
    })
}

/// Summary of one `migrate` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    /// Post-run classification (`nothing_to_do` / `already_migrated` when
    /// no copy ran; `ready` when this call completed the move).
    pub state: PlanState,
    pub copied_files: u64,
    pub copied_bytes: u64,
}

/// Journal persisted beside the destination before the first mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigrationJournal {
    version: u32,
    source: String,
    destination: String,
    /// `"in_progress"` until the verify pass completes.
    status: String,
    /// Unix epoch seconds when the migration started.
    started_at: u64,
}

impl MigrationJournal {
    fn write_atomic(&self, path: &Path) -> Result<(), MigrationError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| MigrationError::Io(e.to_string()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| MigrationError::Io(format!("journal serialize: {e}")))?;
        let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        fs::write(&tmp, text).map_err(|e| MigrationError::Io(e.to_string()))?;
        fs::rename(&tmp, path).map_err(|e| MigrationError::Io(e.to_string()))?;
        Ok(())
    }
}

/// Unix epoch seconds (operational timestamps only — never ledger state).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Idempotent copy: skip files already present with an identical digest;
/// copy everything else through a fsynced `<name>.part-<pid>` sibling.
fn copy_pending(paths: &MigrationPaths) -> Result<(u64, u64), MigrationError> {
    let source_files =
        walk_tree(&paths.source).map_err(|e| MigrationError::Io(format!("walk source: {e}")))?;
    let mut copied_files = 0u64;
    let mut copied_bytes = 0u64;
    for (rel, digest) in &source_files {
        let dest_file = paths.destination.join(rel);
        if same_digest(&dest_file, digest) {
            continue;
        }
        let source_file = paths.source.join(rel);
        if let Some(parent) = dest_file.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| MigrationError::Io(format!("create {}: {e}", parent.display())))?;
        }
        let bytes = copy_verified(&source_file, &dest_file)?;
        copied_files += 1;
        copied_bytes += bytes;
    }
    Ok((copied_files, copied_bytes))
}

/// Copy one file via temp sibling + fsync + rename; returns bytes written.
fn copy_verified(source: &Path, dest: &Path) -> Result<u64, MigrationError> {
    let tmp = dest.with_file_name(format!(
        ".{}.part-{}",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let bytes = fs::copy(source, &tmp)
        .map_err(|e| MigrationError::Io(format!("copy {}: {e}", source.display())))?;
    let file = fs::File::open(&tmp)
        .map_err(|e| MigrationError::Io(format!("open {}: {e}", tmp.display())))?;
    file.sync_all()
        .map_err(|e| MigrationError::Io(format!("fsync {}: {e}", tmp.display())))?;
    drop(file);
    fs::rename(&tmp, dest)
        .map_err(|e| MigrationError::Io(format!("rename {}: {e}", dest.display())))?;
    Ok(bytes)
}

/// Final verify: destination must mirror the source file-for-file.
fn verify(paths: &MigrationPaths) -> Result<(), MigrationError> {
    let source_files =
        walk_tree(&paths.source).map_err(|e| MigrationError::Io(format!("walk source: {e}")))?;
    for (rel, digest) in &source_files {
        let dest_file = paths.destination.join(rel);
        if !same_digest(&dest_file, digest) {
            return Err(MigrationError::Verify(format!(
                "{} does not match {}",
                dest_file.display(),
                paths.source.join(rel).display()
            )));
        }
    }
    Ok(())
}

/// Recursively list `(relative path, blake3 hex digest)` for every regular
/// file, sorted by relative path for deterministic ordering.
fn walk_tree(root: &Path) -> std::io::Result<Vec<FileDigest>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                out.push((rel, digest_file(&path)?));
            }
            // Symlinks are neither followed nor migrated — models trees hold
            // regular GGUF/manifest files only.
        }
    }
    out.sort();
    Ok(out)
}

fn same_digest(path: &Path, digest: &str) -> bool {
    path.is_file() && digest_file(path).map(|d| d == digest).unwrap_or(false)
}

fn digest_file(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, contents).expect("write");
    }

    fn paths(home: &tempfile::TempDir) -> MigrationPaths {
        models_paths(home.path())
    }

    #[test]
    fn fresh_install_has_nothing_to_do() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(preflight(&paths(&home)).state, PlanState::NothingToDo);
    }

    #[test]
    fn ready_counts_pending_work() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "qwen.gguf", b"weights");
        seed(&home.path().join("models"), "sub/bge.gguf", b"embeddings!");
        let plan = preflight(&paths(&home));
        assert_eq!(plan.state, PlanState::Ready);
        assert_eq!(plan.files_to_copy, 2);
        assert_eq!(plan.bytes_to_copy, 7 + 11);
    }

    #[test]
    fn migrate_copies_tree_and_marks_journal_complete() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "a.gguf", b"AAA");
        seed(&home.path().join("models"), "nested/b.gguf", b"BBB");
        let report = migrate(&paths(&home)).unwrap();
        assert_eq!(report.copied_files, 2);
        assert_eq!(report.copied_bytes, 6);
        assert!(
            home.path().join("brain/models/a.gguf").is_file(),
            "canonical destination populated"
        );
        let journal =
            fs::read_to_string(home.path().join("brain/.models.migration-journal.json")).unwrap();
        assert!(journal.contains("\"complete\""), "{journal}");
        // Source is preserved as the recoverable backup.
        assert!(home.path().join("models/a.gguf").is_file());
    }

    #[test]
    fn rerun_after_complete_is_already_migrated() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "a.gguf", b"AAA");
        migrate(&paths(&home)).unwrap();
        let report = migrate(&paths(&home)).unwrap();
        assert_eq!(report.state, PlanState::AlreadyMigrated);
        assert_eq!(report.copied_files, 0);
    }

    #[test]
    fn resume_after_partial_copy_completes_without_recopy() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "a.gguf", b"AAA");
        seed(&home.path().join("models"), "b.gguf", b"BBB");
        // Simulate a crash after one file landed: pre-copy b, then migrate.
        seed(&home.path().join("brain/models"), "b.gguf", b"BBB");
        let report = migrate(&paths(&home)).unwrap();
        assert_eq!(report.copied_files, 1, "only the missing file is copied");
        assert!(home.path().join("brain/models/a.gguf").is_file());
    }

    #[test]
    fn conflicting_content_refuses_and_touches_nothing() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "a.gguf", b"new");
        seed(&home.path().join("brain/models"), "a.gguf", b"old");
        let err = migrate(&paths(&home)).unwrap_err();
        let MigrationError::Conflict { old, new } = err else {
            panic!("expected conflict");
        };
        assert_eq!(old, home.path().join("models"));
        assert_eq!(new, home.path().join("brain/models"));
        assert_eq!(fs::read(home.path().join("models/a.gguf")).unwrap(), b"new");
        assert_eq!(
            fs::read(home.path().join("brain/models/a.gguf")).unwrap(),
            b"old"
        );
        assert!(
            !home
                .path()
                .join("brain/.models.migration-journal.json")
                .exists()
        );
    }

    #[test]
    fn extra_destination_file_is_a_conflict() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "a.gguf", b"AAA");
        seed(&home.path().join("brain/models"), "stray.gguf", b"???");
        assert_eq!(preflight(&paths(&home)).state, PlanState::Conflict);
    }

    #[test]
    fn dry_run_preflight_never_mutates() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("models"), "a.gguf", b"AAA");
        let plan = preflight(&paths(&home));
        assert_eq!(plan.state, PlanState::Ready);
        assert_eq!(plan.files_to_copy, 1);
        assert!(
            !home.path().join("brain").exists(),
            "preflight must not create the destination"
        );
    }

    #[test]
    fn source_missing_destination_populated_is_already_migrated() {
        let home = tempfile::tempdir().unwrap();
        seed(&home.path().join("brain/models"), "a.gguf", b"AAA");
        assert_eq!(preflight(&paths(&home)).state, PlanState::AlreadyMigrated);
        // And running it is a safe no-op.
        let report = migrate(&paths(&home)).unwrap();
        assert_eq!(report.state, PlanState::AlreadyMigrated);
    }
}
