//! Read-only gix-backed document reader for the two-plane document cache.
//!
//! This module never writes to disk: it never touches the index, never locks
//! the config, and never stages files. It opens the repository via `gix::open`,
//! descends HEAD trees, reads blobs, and walks commit history purely for read.
//!
//! The writer side lives in the separate `oxi-vault-git` crate; we only consume
//! the commits it produces. Test repos in `tests/git_docs.rs` mirror the
//! `oxi-vault-git::create_initial_commit` pattern (write blob → tree → commit
//! directly, leave the index empty) so that this module can read them in the
//! same shape users will have on disk.
//!
//! ## Revision identity
//!
//! - `git:<format>:<oid>` — the worktree bytes equal the HEAD blob. Identical
//!   bytes ⇒ identical oid ⇒ cache hits, no re-ingest.
//! - `blake3:<hex>` — the worktree bytes differ (dirty), the file is untracked,
//!   or the file is outside the HEAD tree. Cache stores the blake3 digest so
//!   subsequent materialization can verify byte equality without re-reading
//!   the full file.
//!
//! ## Ignore matching
//!
//! The primary path builds a gix attribute stack and asks it whether the
//! locator is excluded (honoring root + nested `.gitignore`, `info/exclude`,
//! and the configured `core.excludesFile`). If that path is unavailable — the
//! gix 0.83 exclude platform requires an `AttributeStack` plus an index file,
//! and a freshly-initialized repo with no `.git/index` trips a few edge cases
//! in practice — we fall back to reading the root-level `.gitignore` and
//! matching with the `glob` crate. The fallback is logged so operators can
//! spot misconfigurations in their `.gitignore`.
//!
//! ## Rename detection
//!
//! `rename_hint` walks parent history of the latest commit that touched
//! `old_locator`. If that commit (or one of its ancestors that still affects
//! `old_locator`) is missing the path but contains a new path with a blob
//! matching the oid of the last-seen `old_locator` blob, the new path is
//! returned. When more than one candidate new path matches, the function
//! returns `None` (ambiguity) rather than guess — rename accuracy over recall
//! is the right tradeoff for the document plane.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use gix::bstr::BStr;
use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use oxibrain_ports::BrainError;

/// Read-only gix-backed document reader.
///
/// Cloning is cheap (the inner `gix::Repository` is reference-counted), so
/// callers can hand the reader around freely without paying for a re-open.
pub struct GitDocumentReader {
    repo: gix::Repository,
}

/// A blob identified by its oid plus its raw byte length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBlob {
    pub oid: String,
    pub bytes: u64,
}

/// A read-only snapshot of HEAD: which blob is tracked at each root-relative
/// locator, plus the resolved HEAD commit id and the repo's object format.
///
/// `tracked` is sorted by locator for deterministic output across runs.
#[derive(Debug, Clone)]
pub struct GitSnapshot {
    pub head: Option<String>,
    pub object_format: String,
    pub tracked: BTreeMap<String, GitBlob>,
}

/// A historical revision of a tracked document.
///
/// `revision` is the canonical `git:<format>:<oid>` form. `committed_at` is the
/// committer time in seconds since the UNIX epoch.
#[derive(Debug, Clone)]
pub struct DocumentRevision {
    pub root_alias: String,
    pub locator: String,
    pub revision: String,
    pub content: Vec<u8>,
    pub committed_at: i64,
}

impl GitDocumentReader {
    /// Open a reader for the worktree at `root`.
    ///
    /// Returns `Ok(None)` when the path is not inside a git worktree — the
    /// document plane treats plain roots as "no history, blake3 everything".
    /// Returns `Err(BrainError::Corruption)` for a corrupt `.git` directory
    /// because that usually indicates operator-level intervention (partial
    /// restore, manual edits) that warrants a loud failure.
    pub fn open(root: &Path) -> Result<Option<Self>, BrainError> {
        match gix::open(root) {
            Ok(repo) => Ok(Some(Self { repo })),
            Err(err) => {
                if is_not_a_repo(&err) {
                    Ok(None)
                } else {
                    Err(BrainError::Corruption(format!(
                        "failed to open git repository at {}: {err}",
                        root.display()
                    )))
                }
            }
        }
    }

    /// Snapshot HEAD: commit id, object format, and every tracked blob keyed by
    /// its root-relative locator.
    pub fn snapshot(&self) -> Result<GitSnapshot, BrainError> {
        let head_id = self.repo.head_id().ok().map(|id| id.detach());
        let object_format = self.repo.object_hash().to_string();

        let mut tracked = BTreeMap::new();
        if let Some(head_oid) = head_id {
            let commit = self.repo.find_commit(head_oid).map_err(gix_err)?;
            let decoded = commit.decode().map_err(gix_err)?;
            let tree_id = decoded.tree();
            self.collect_tree(tree_id, "", &mut tracked)?;
        }

        Ok(GitSnapshot {
            head: head_id.map(|id| id.to_hex().to_string()),
            object_format,
            tracked,
        })
    }

    /// Recursively descend `tree_id`, writing `(locator, GitBlob)` entries
    /// into `out`. Subtrees are recursed with their relative prefix joined
    /// via `/` (matches the locator convention used by the rest of the
    /// document plane). Non-blob entries (submodules, symlinks) are skipped
    /// — the document plane only deals with regular files.
    fn collect_tree(
        &self,
        tree_id: ObjectId,
        prefix: &str,
        out: &mut BTreeMap<String, GitBlob>,
    ) -> Result<(), BrainError> {
        let tree = self.repo.find_tree(tree_id).map_err(gix_err)?;
        let decoded = tree.decode().map_err(gix_err)?;
        for entry in decoded.entries {
            let name = entry
                .filename
                .to_str()
                .map_err(|_| BrainError::Corruption("non-utf8 path component in tree".into()))?;
            let locator = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            match entry.mode.kind() {
                EntryKind::Tree => self.collect_tree(entry.oid.to_owned(), &locator, out)?,
                EntryKind::Blob | EntryKind::BlobExecutable => {
                    let blob = self.repo.find_blob(entry.oid).map_err(gix_err)?;
                    out.insert(
                        locator,
                        GitBlob {
                            oid: entry.oid.to_hex().to_string(),
                            bytes: blob.data.len() as u64,
                        },
                    );
                }
                _ => {
                    // Submodule, symlink, etc. — not part of the document plane.
                }
            }
        }
        Ok(())
    }

    /// True when `locator` is excluded by the repository's ignore rules
    /// (root + nested `.gitignore`, `info/exclude`, `core.excludesFile`).
    ///
    /// Falls back to a root-level `.gitignore` glob match if the gix exclude
    /// platform can't be built (missing index file on a fresh repo, etc.) —
    /// the fallback is logged once so operators can investigate.
    pub fn is_ignored(&self, locator: &str) -> Result<bool, BrainError> {
        match self.is_ignored_via_gix(locator) {
            Ok(answer) => Ok(answer),
            Err(err) => {
                tracing::warn!(
                    locator,
                    error = %err,
                    "gix exclude platform unavailable; falling back to root .gitignore glob"
                );
                Ok(self.is_ignored_via_root_gitignore(locator))
            }
        }
    }

    fn is_ignored_via_gix(&self, locator: &str) -> Result<bool, BrainError> {
        let index = self.repo.index_or_empty().map_err(gix_err)?;
        let mut stack = self
            .repo
            .excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
            .map_err(|err| BrainError::Corruption(format!("excludes stack: {err}")))?;
        let platform = stack
            .at_path(locator, None)
            .map_err(|err| BrainError::Corruption(format!("ignore platform: {err}")))?;
        Ok(platform.is_excluded())
    }

    fn is_ignored_via_root_gitignore(&self, locator: &str) -> bool {
        let Some(workdir) = self.repo.workdir() else {
            return false;
        };
        let gitignore = workdir.join(".gitignore");
        let Ok(bytes) = fs::read(&gitignore) else {
            return false;
        };
        let text = String::from_utf8_lossy(&bytes);
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (pattern, negated) = match line.strip_prefix('!') {
                Some(inner) => (inner, true),
                None => (line, false),
            };
            if let Ok(pat) = glob::Pattern::new(pattern) {
                let opts = glob::MatchOptions {
                    case_sensitive: true,
                    require_literal_separator: false,
                    require_literal_leading_dot: false,
                };
                if pat.matches_with(locator, opts) {
                    return !negated;
                }
            }
        }
        false
    }

    /// Resolve a blob oid to its raw bytes.
    pub fn blob_bytes(&self, oid: &str) -> Result<Vec<u8>, BrainError> {
        let id = ObjectId::from_hex(oid.as_bytes())
            .map_err(|err| BrainError::Invalid(format!("invalid blob oid {oid:?}: {err}")))?;
        let blob = self.repo.find_blob(id).map_err(|err| {
            if is_not_found(&err) {
                BrainError::NotFound(format!("blob {oid}"))
            } else {
                gix_err(err)
            }
        })?;
        Ok(blob.data.to_vec())
    }

    /// Compute the canonical revision tag for the worktree bytes of `locator`.
    ///
    /// - When `snapshot.tracked` already carries a blob for `locator` AND the
    ///   supplied `worktree_bytes` hash to that same oid, return
    ///   `git:<format>:<oid>`. The worktree is clean and the blob is canonical.
    /// - Otherwise return `blake3:<hex>` of `worktree_bytes`. The worktree is
    ///   dirty, the file is untracked, or the file is outside the HEAD tree.
    pub fn current_revision(
        &self,
        snapshot: &GitSnapshot,
        locator: &str,
        worktree_bytes: &[u8],
    ) -> Result<String, BrainError> {
        let format = snapshot.object_format.as_str();
        if let Some(tracked) = snapshot.tracked.get(locator) {
            let worktree_oid = write_blob_oid(&self.repo, worktree_bytes)?;
            if worktree_oid == tracked.oid {
                return Ok(format!("git:{format}:{}", tracked.oid));
            }
        }
        Ok(format!("blake3:{}", blake3_hex(worktree_bytes)))
    }

    /// Commit history for `locator`, oldest first, capped at `limit`.
    ///
    /// Only commits where the locator exists in the tree are surfaced; rename
    /// history is not followed — callers use `rename_hint` for that. Each
    /// returned [`DocumentRevision`] carries the file bytes as they existed
    /// at that commit, so callers can render history without further I/O.
    pub fn history(
        &self,
        root_alias: &str,
        locator: &str,
        limit: usize,
    ) -> Result<Vec<DocumentRevision>, BrainError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let Some(head_id) = self.repo.head_id().ok().map(|id| id.detach()) else {
            return Ok(Vec::new());
        };
        let format = self.repo.object_hash().to_string();

        // Walk newest→oldest, buffering (commit time, blob) per commit so we
        // can diff each commit against its parent — a commit belongs in the
        // history exactly when its blob differs from the parent's (or the
        // path is absent in the parent), mirroring `git log -- <path>`.
        let mut timeline: Vec<(i64, Option<ObjectId>)> = Vec::new();
        for info in self
            .repo
            .rev_walk([head_id])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(|err| BrainError::Corruption(format!("rev walk: {err}")))?
        {
            let info =
                info.map_err(|err| BrainError::Corruption(format!("rev walk iter: {err}")))?;
            let commit = info.object().map_err(gix_err)?;
            let decoded = commit.decode().map_err(gix_err)?;
            let tree_id = decoded.tree();
            let blob = find_blob_in_tree(&self.repo, tree_id, locator).ok();
            let committed_at = decoded
                .committer()
                .map_err(gix_err)?
                .time()
                .map_err(gix_err)?
                .seconds;
            timeline.push((committed_at, blob));
        }

        // Pair each commit with its parent (the next entry in the walk);
        // record when the blob changed. Stop as soon as `limit` is reached.
        // `walk_pos` (0 = walk start = newest) makes the final ordering
        // deterministic even when two commits share a timestamp.
        let mut commits: Vec<(usize, DocumentRevision)> = Vec::new();
        for i in 0..timeline.len() {
            let (committed_at, blob) = timeline[i];
            let parent_blob = timeline.get(i + 1).and_then(|(_, b)| *b);
            if blob == parent_blob {
                continue; // unchanged in this commit
            }
            let Some(blob_id) = blob else {
                continue; // path absent here; the deletion itself is not a revision
            };
            let content = self.repo.find_blob(blob_id).map_err(gix_err)?.data.to_vec();
            commits.push((
                i,
                DocumentRevision {
                    root_alias: root_alias.to_string(),
                    locator: locator.to_string(),
                    revision: format!("git:{format}:{}", blob_id.to_hex()),
                    content,
                    committed_at,
                },
            ));
            if commits.len() >= limit {
                break;
            }
        }
        // Oldest first: ascending commit time; among equal timestamps the
        // later walk position (older commit in a newest-first walk) wins.
        commits.sort_by(|(pa, a), (pb, b)| a.committed_at.cmp(&b.committed_at).then(pb.cmp(pa)));
        Ok(commits.into_iter().map(|(_, rev)| rev).collect())
    }

    /// Best-effort rename hint: the new locator where the identical blob
    /// previously residing at `old_locator` last appeared.
    ///
    /// Walks commit history newest→oldest. The first commit where
    /// `old_locator` is missing from the tree is the rename candidate; the
    /// blob identity comes from the next-older commit where the path still
    /// existed. We then look for paths in the candidate tree carrying that
    /// same blob oid: exactly one match ⇒ the new locator, zero or several ⇒
    /// `None` (refuse to guess).
    pub fn rename_hint(&self, old_locator: &str) -> Result<Option<String>, BrainError> {
        let Some(head_id) = self.repo.head_id().ok().map(|id| id.detach()) else {
            return Ok(None);
        };

        let mut candidate_tree: Option<ObjectId> = None;
        let mut last_blob: Option<ObjectId> = None;
        for info in self
            .repo
            .rev_walk([head_id])
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(|err| BrainError::Corruption(format!("rev walk: {err}")))?
        {
            let info =
                info.map_err(|err| BrainError::Corruption(format!("rev walk iter: {err}")))?;
            let commit = info.object().map_err(gix_err)?;
            let decoded = commit.decode().map_err(gix_err)?;
            let tree_id = decoded.tree();
            match find_blob_in_tree(&self.repo, tree_id, old_locator) {
                Ok(oid) => {
                    // The path still exists here — the last commit that
                    // carried it. The blob identity is now known. Stop.
                    last_blob = Some(oid);
                    break;
                }
                Err(_) if candidate_tree.is_none() => {
                    // First commit where the path is gone: rename candidate.
                    candidate_tree = Some(tree_id);
                }
                Err(_) => {}
            }
        }

        let Some(blob_id) = last_blob else {
            return Ok(None); // path never existed (or repo empty)
        };
        let Some(tree_id) = candidate_tree else {
            return Ok(None); // path still present at HEAD — nothing renamed away
        };

        let mut candidates = Vec::new();
        self.collect_matching_blobs(tree_id, "", blob_id, &mut candidates)?;
        match candidates.len() {
            1 => Ok(Some(std::mem::take(&mut candidates[0]))),
            _ => Ok(None),
        }
    }

    /// Walk the tree at `tree_id` and record every locator whose blob oid
    /// equals `target`. Skips submodules/symlinks. Excludes `old_locator`
    /// indirectly because the caller stops walking at the first commit where
    /// `old_locator` is missing — so we only traverse the rename-candidate
    /// commit's tree, which by definition no longer contains the old path.
    fn collect_matching_blobs(
        &self,
        tree_id: ObjectId,
        prefix: &str,
        target: ObjectId,
        out: &mut Vec<String>,
    ) -> Result<(), BrainError> {
        let tree = self.repo.find_tree(tree_id).map_err(gix_err)?;
        let decoded = tree.decode().map_err(gix_err)?;
        for entry in decoded.entries {
            let name = entry
                .filename
                .to_str()
                .map_err(|_| BrainError::Corruption("non-utf8 path component in tree".into()))?;
            let locator = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            match entry.mode.kind() {
                EntryKind::Tree => {
                    self.collect_matching_blobs(entry.oid.to_owned(), &locator, target, out)?;
                }
                EntryKind::Blob | EntryKind::BlobExecutable if entry.oid == target => {
                    out.push(locator);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
/// Compute the blob oid that `gix` would assign to `bytes`, using the
/// repository's configured object format. Pure compute — no odb write,
/// no index touch, no config lock — so it's safe for the read-only reader.
fn write_blob_oid(repo: &gix::Repository, bytes: &[u8]) -> Result<String, BrainError> {
    let oid = gix::objs::compute_hash(repo.object_hash(), gix::objs::Kind::Blob, bytes)
        .map_err(|err| BrainError::Corruption(format!("compute_hash: {err}")))?;
    Ok(oid.to_hex().to_string())
}

// --- free helpers -----------------------------------------------------------

/// Descend through `tree_id` looking for the entry at `rel_path`. Returns the
/// blob oid on success, [`BrainError::NotFound`] if any component is missing.
/// Subtrees are followed in order; the final component must resolve to a
/// blob entry (the caller decides what to do with non-blobs).
fn find_blob_in_tree(
    repo: &gix::Repository,
    tree_id: ObjectId,
    rel_path: &str,
) -> Result<ObjectId, BrainError> {
    let components: Vec<&str> = Path::new(rel_path)
        .iter()
        .filter_map(|c| c.to_str())
        .collect();
    if components.is_empty() {
        return Err(BrainError::Invalid(format!("empty path: {rel_path}")));
    }
    let mut current = tree_id;
    for (i, component) in components.iter().enumerate() {
        let tree = repo.find_tree(current).map_err(gix_err)?;
        let decoded = tree.decode().map_err(gix_err)?;
        let needle = BStr::new(component);
        let entry = decoded
            .entries
            .iter()
            .find(|e| e.filename == needle)
            .ok_or_else(|| BrainError::NotFound(format!("path component {component:?}")))?;
        if i + 1 == components.len() {
            return Ok(entry.oid.to_owned());
        }
        current = entry.oid.to_owned();
    }
    Err(BrainError::Invalid(format!(
        "unreachable tree walk: {rel_path}"
    )))
}

/// Hex-encode the blake3 digest of `bytes`.
fn blake3_hex(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    hex::encode(hash.as_bytes())
}

/// Map an arbitrary gix error into a [`BrainError::Corruption`] for the
/// reader's read-only surface. Read errors that mean "not present" are
/// surfaced as `BrainError::NotFound` so callers can distinguish them from
/// genuinely corrupt repositories.
fn gix_err<E: std::fmt::Display>(err: E) -> BrainError {
    BrainError::Corruption(err.to_string())
}

/// A gix open failure that simply means "no repository here" (nothing
/// discovered at or above the path). Anything else — corrupt config, unsafe
/// ownership, I/O failure — is surfaced as corruption.
fn is_not_a_repo(err: &gix::open::Error) -> bool {
    matches!(err, gix::open::Error::NotARepository { .. })
}

/// Heuristic: gix reports missing blobs with "not found" somewhere in the
/// display string.
fn is_not_found<E: std::fmt::Display>(err: &E) -> bool {
    err.to_string().contains("not found")
}
