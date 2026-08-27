//! Integration tests for the read-only gix document reader.
//!
//! Repos are created with direct gix blob/tree/commit writes and an EMPTY
//! index — mirroring `oxi-vault-git::create_initial_commit` — never `git add`.
//! This exercises exactly the repository shape oxibrain must read: index-less
//! worktrees whose HEAD trees are maintained by the oxi writer.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use gix::hash::ObjectId;
use gix::objs::tree::EntryKind;
use oxibrain_connectors::GitDocumentReader;
use tempfile::TempDir;

/// Committer signature borrowing the caller-owned raw `<seconds> +0000`
/// string, so commit timestamps are deterministic without leaking.
fn sig(time: &str) -> gix::actor::SignatureRef<'_> {
    gix::actor::SignatureRef {
        name: "oxibrain-test".into(),
        email: "oxibrain-test@example.com".into(),
        time,
    }
}

/// Test harness: an index-less repo whose commits are written directly via
/// gix (blob → tree → commit), mirroring `oxi-vault-git::create_initial_commit`.
/// HEAD parent tracking is required because `commit_as` uses a
/// `MustNotExist`/`ExistingMustMatch` ref transaction for parentless/further
/// commits respectively.
struct Repo {
    repo: gix::Repository,
    ref_name: String,
    head: Option<ObjectId>,
}

impl Repo {
    /// Init a repo at `dir`, discovering the branch unborn HEAD points at
    /// (respects the machine's init.defaultBranch — never assume "main").
    fn init(dir: &Path) -> Repo {
        let repo = gix::init(dir).unwrap();
        let ref_name = repo
            .head_name()
            .unwrap()
            .expect("unborn HEAD still names a branch")
            .to_string();
        Repo {
            repo,
            ref_name,
            head: None,
        }
    }

    /// Write one commit: `files` is the complete tree state, built from the
    /// empty tree so removed paths simply vanish. The index is never touched.
    fn commit(&mut self, files: &[(&str, &[u8])], seconds: i64, message: &str) -> String {
        let empty_tree = ObjectId::empty_tree(self.repo.object_hash());
        let mut editor = self.repo.edit_tree(empty_tree).unwrap();
        for (path, content) in files {
            let blob_id = self.repo.write_blob(content).unwrap();
            editor.upsert(*path, EntryKind::Blob, blob_id).unwrap();
        }
        let tree_id = editor.write().unwrap();
        let raw_time = format!("{seconds} +0000");
        let s = sig(&raw_time);
        let parents: Vec<ObjectId> = self.head.into_iter().collect();
        let commit_id = self
            .repo
            .commit_as(
                s,
                s,
                self.ref_name.as_str(),
                message,
                tree_id.detach(),
                parents,
            )
            .unwrap();
        let detached = commit_id.detach();
        self.head = Some(detached);
        detached.to_hex().to_string()
    }
}

// --- open -------------------------------------------------------------------

#[test]
fn non_repo_yields_none() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("plain.md"), "no git here").unwrap();
    assert!(GitDocumentReader::open(dir.path()).unwrap().is_none());
}

#[test]
fn empty_repo_snapshots_empty() {
    let dir = TempDir::new().unwrap();
    let _repo = Repo::init(dir.path());
    let reader = GitDocumentReader::open(dir.path())
        .unwrap()
        .expect("fresh repo opens");

    let snap = reader.snapshot().unwrap();
    assert!(snap.head.is_none());
    assert!(snap.tracked.is_empty());
    assert_eq!(snap.object_format, "sha1");
    assert!(reader.history("r", "a.md", 10).unwrap().is_empty());
    assert!(reader.rename_hint("a.md").unwrap().is_none());
}

// --- snapshot ----------------------------------------------------------------

#[test]
fn snapshot_lists_head_files() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(
        &[("a.md", b"# alpha"), ("nested/deep/b.md", b"# beta beta")],
        1_000,
        "initial",
    );

    let reader = GitDocumentReader::open(dir.path()).unwrap().unwrap();
    let snap = reader.snapshot().unwrap();

    assert!(snap.head.is_some());
    assert_eq!(snap.object_format, "sha1");
    assert_eq!(snap.tracked.len(), 2);

    let a = &snap.tracked["a.md"];
    assert_eq!(a.bytes, 7);
    // The oid must round-trip through blob_bytes to the exact content.
    assert_eq!(reader.blob_bytes(&a.oid).unwrap(), b"# alpha");

    let nested = &snap.tracked["nested/deep/b.md"];
    assert_eq!(nested.bytes, 11);
    assert_eq!(reader.blob_bytes(&nested.oid).unwrap(), b"# beta beta");

    // Deterministic, locator-sorted output.
    let keys: Vec<&String> = snap.tracked.keys().collect();
    assert_eq!(keys, ["a.md", "nested/deep/b.md"]);
}

// --- current_revision ---------------------------------------------------------

#[test]
fn clean_file_gets_git_revision_dirty_gets_blake3() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(&[("a.md", b"clean bytes")], 1_000, "c1");

    let reader = GitDocumentReader::open(dir.path()).unwrap().unwrap();
    let snap = reader.snapshot().unwrap();

    // Clean: identical bytes ⇒ git:<format>:<oid>.
    let clean = reader
        .current_revision(&snap, "a.md", b"clean bytes")
        .unwrap();
    let expected_oid = &snap.tracked["a.md"].oid;
    assert_eq!(clean, format!("git:sha1:{expected_oid}"));

    // Dirty: changed bytes ⇒ blake3:<hex> of the worktree bytes.
    let dirty = reader
        .current_revision(&snap, "a.md", b"edited bytes")
        .unwrap();
    let expected = format!(
        "blake3:{}",
        hex::encode(blake3::hash(b"edited bytes").as_bytes())
    );
    assert_eq!(dirty, expected);

    // Untracked locator ⇒ blake3 as well.
    let untracked = reader
        .current_revision(&snap, "new-file.md", b"brand new")
        .unwrap();
    assert!(untracked.starts_with("blake3:"));
}

// --- history ------------------------------------------------------------------

#[test]
fn history_is_oldest_first_with_content() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(&[("doc.md", b"version one")], 1_000, "v1");
    repo.commit(&[("doc.md", b"version two")], 2_000, "v2");
    // A commit that does NOT change doc.md must not add a history entry.
    repo.commit(
        &[("doc.md", b"version two"), ("z.md", b"zz")],
        3_000,
        "other",
    );

    let reader = GitDocumentReader::open(dir.path()).unwrap().unwrap();
    let hist = reader.history("root", "doc.md", 10).unwrap();

    assert_eq!(hist.len(), 2, "unchanged commit yields no entry: {hist:?}");
    // Oldest first.
    assert_eq!(hist[0].content, b"version one");
    assert_eq!(hist[0].committed_at, 1_000);
    assert_eq!(hist[1].content, b"version two");
    assert_eq!(hist[1].committed_at, 2_000);
    // Revision form + pass-through fields.
    assert!(hist[0].revision.starts_with("git:sha1:"));
    assert_eq!(hist[0].root_alias, "root");
    assert_eq!(hist[0].locator, "doc.md");

    // Limit keeps the N most recent revisions (git log -n semantics),
    // still rendered oldest-first.
    let limited = reader.history("root", "doc.md", 1).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].content, b"version two");
}

// --- ignores ------------------------------------------------------------------

#[test]
fn ignored_locator_is_reported_ignored() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(&[("a.md", b"tracked")], 1_000, "c1");
    fs::write(dir.path().join(".gitignore"), "*.ign.md\n").unwrap();
    fs::create_dir_all(dir.path().join("notes")).unwrap();
    fs::write(dir.path().join("notes/draft.ign.md"), "scratch").unwrap();

    let reader = GitDocumentReader::open(dir.path()).unwrap().unwrap();
    assert!(
        reader.is_ignored("notes/draft.ign.md").unwrap(),
        "ignored pattern must match nested locator"
    );
    assert!(!reader.is_ignored("a.md").unwrap());
}

// --- rename hint ---------------------------------------------------------------

#[test]
fn rename_hint_finds_identical_blob_rename() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(&[("old.md", b"same payload")], 1_000, "add");
    repo.commit(&[("new.md", b"same payload")], 2_000, "rename");

    let reader = GitDocumentReader::open(dir.path()).unwrap().unwrap();
    assert_eq!(
        reader.rename_hint("old.md").unwrap().as_deref(),
        Some("new.md")
    );
}

#[test]
fn rename_hint_ambiguous_returns_none() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(&[("old.md", b"same payload")], 1_000, "add");
    repo.commit(
        &[("left.md", b"same payload"), ("right.md", b"same payload")],
        2_000,
        "fork",
    );

    let reader = GitDocumentReader::open(dir.path()).unwrap().unwrap();
    assert_eq!(reader.rename_hint("old.md").unwrap(), None);
}

// --- read-only .git ------------------------------------------------------------

#[test]
fn read_only_git_dir_still_opens() {
    let dir = TempDir::new().unwrap();
    let mut repo = Repo::init(dir.path());
    repo.commit(&[("a.md", b"content")], 1_000, "c1");

    let git_dir = dir.path().join(".git");
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o500)).unwrap();

    let opened = GitDocumentReader::open(dir.path()).unwrap();
    assert!(opened.is_some(), "read-only .git must not block the reader");
    let reader = opened.unwrap();
    let snap = reader.snapshot().unwrap();
    assert!(snap.tracked.contains_key("a.md"));
    assert!(!reader.history("r", "a.md", 5).unwrap().is_empty());

    // Restore so TempDir's drop can clean up.
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o755)).unwrap();
}
