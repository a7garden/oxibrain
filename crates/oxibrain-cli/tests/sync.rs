//! End-to-end test for `oxibrain sync` — the CLI is a first-class product
//! surface (ARCHITECTURE.md §16.4), so the contract is tested through the
//! binary itself: idempotent re-sync, modified-file detection, and episode
//! count conservation on unchanged re-syncs.

use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn run_oxibrain(store: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .arg("--dir")
        .arg(store)
        .args(args)
        .output()
        .expect("spawn oxibrain");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn write_vault_file(vault: &std::path::Path, rel: &str, content: &str) {
    let path = vault.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

fn episode_count(store: &std::path::Path) -> i64 {
    let (ok, out) = run_oxibrain(store, &["stats"]);
    assert!(ok, "stats failed: {out}");
    out.lines()
        .find_map(|l| l.strip_prefix("episodes: "))
        .expect("episodes line")
        .trim()
        .parse()
        .expect("episode count")
}

#[test]
fn sync_is_idempotent_and_detects_modifications() {
    let store = tempdir().expect("store dir");
    let vault = tempdir().expect("vault dir");

    write_vault_file(
        vault.path(),
        "a.md",
        "# Conventions\n\nCommit style: conventional commits.",
    );
    write_vault_file(
        vault.path(),
        "sub/b.md",
        "# Deploy\n\nUse `bun` for the frontend.",
    );

    // First sync: both files are new.
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "first sync failed: {out}");
    assert!(
        out.contains("sync complete: 2 new, 0 unchanged, 0 modified"),
        "unexpected report: {out}"
    );
    assert_eq!(episode_count(store.path()), 2);

    // Second sync of an unchanged tree: no-op.
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "second sync failed: {out}");
    assert!(
        out.contains("sync complete: 0 new, 2 unchanged, 0 modified"),
        "unexpected report: {out}"
    );
    assert_eq!(
        episode_count(store.path()),
        2,
        "unchanged re-sync must not append episodes"
    );

    // Modify one file: it is Modified, the other stays Unchanged.
    write_vault_file(
        vault.path(),
        "a.md",
        "# Conventions\n\nCommit style: conventional commits. Branches: type/short-description.",
    );
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "third sync failed: {out}");
    assert!(
        out.contains("sync complete: 0 new, 1 unchanged, 1 modified"),
        "unexpected report: {out}"
    );
    assert!(
        out.contains("a.md"),
        "modified path should be listed: {out}"
    );
    assert_eq!(episode_count(store.path()), 3);

    // Fourth sync: the modified content is now known — stable again.
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "fourth sync failed: {out}");
    assert!(
        out.contains("sync complete: 0 new, 2 unchanged, 0 modified"),
        "unexpected report: {out}"
    );
    assert_eq!(episode_count(store.path()), 3);
}

#[test]
fn sync_rejects_missing_directory() {
    let store = tempdir().expect("store dir");
    let vault = tempdir().expect("vault dir");
    let missing = vault.path().join("nope");
    let (ok, _) = run_oxibrain(store.path(), &["sync", missing.to_str().unwrap()]);
    assert!(!ok, "sync of a missing directory must fail");
}

#[test]
fn sync_is_idempotent_on_rerun() {
    let store = tempdir().expect("store dir");
    let vault = tempdir().expect("vault dir");
    let vault_path = vault.path();

    write_vault_file(vault_path, "x.md", "stable content");

    // First sync: new.
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault_path.to_str().unwrap()]);
    assert!(ok, "first sync failed: {out}");
    assert!(
        out.contains("sync complete: 1 new, 0 unchanged, 0 modified"),
        "unexpected: {out}"
    );
    assert_eq!(episode_count(store.path()), 1);

    // Second sync: unchanged — event-path state matches the file.
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault_path.to_str().unwrap()]);
    assert!(ok, "second sync failed: {out}");
    assert!(
        out.contains("sync complete: 0 new, 1 unchanged, 0 modified"),
        "second sync must report 0 new and 0 modified: {out}"
    );
    assert_eq!(
        episode_count(store.path()),
        1,
        "unchanged re-sync must not append episodes"
    );
}

#[test]
fn occurrence_chain_a_b_a_creates_three_events() {
    let store = tempdir().expect("store dir");
    let vault = tempdir().expect("vault dir");
    let vault_path = vault.path();
    let space_name = "test";
    let locator = "note.md";

    // A
    write_vault_file(vault_path, locator, "version A");
    let (ok, out) = run_oxibrain(
        store.path(),
        &["sync", "--space", space_name, vault_path.to_str().unwrap()],
    );
    assert!(ok, "sync A failed: {out}");
    assert!(
        out.contains("sync complete: 1 new, 0 unchanged, 0 modified"),
        "unexpected: {out}"
    );
    assert_eq!(episode_count(store.path()), 1);

    // Resolve the actual space_id and source_id the CLI registered. Both
    // helpers are idempotent: ensure_space returns the existing truncated
    // blake3 space_id, and ensure_source returns the source_id derived
    // from (space_id, canonical_vault_path). These match what the CLI
    // computed without re-implementing the truncated blake3 here.
    let canonical = std::fs::canonicalize(vault_path).unwrap_or_else(|_| vault_path.to_path_buf());
    let source_name = canonical.to_string_lossy().into_owned();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (space_id, source_id) = runtime.block_on(async {
        let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(store.path()))
            .await
            .expect("open brain");
        let space_id = brain.ensure_space(space_name).await.expect("ensure_space");
        let source_id = brain
            .ensure_source(&space_id, &source_name, "document_revision", "pull")
            .await
            .expect("ensure_source");
        (space_id, source_id)
    });

    // Deterministic occurrence for A: predecessor = None.
    let hash_a = oxibrain_core::content_hash("version A");
    let occ_a1 = oxibrain_core::occurrence_id(&source_id, locator, None, &hash_a);

    // B — the predecessor is occ_a1, so the new occurrence_id differs.
    write_vault_file(vault_path, locator, "version B");
    let (ok, out) = run_oxibrain(
        store.path(),
        &["sync", "--space", space_name, vault_path.to_str().unwrap()],
    );
    assert!(ok, "sync B failed: {out}");
    assert!(
        out.contains("sync complete: 0 new, 0 unchanged, 1 modified"),
        "B must be classified as modified: {out}"
    );
    assert_eq!(episode_count(store.path()), 2);

    let hash_b = oxibrain_core::content_hash("version B");
    let occ_b = oxibrain_core::occurrence_id(&source_id, locator, Some(&occ_a1), &hash_b);

    // A again — must be classified as Modified (predecessor = occ_b, not None),
    // creating a THIRD distinct event.
    write_vault_file(vault_path, locator, "version A");
    let (ok, out) = run_oxibrain(
        store.path(),
        &["sync", "--space", space_name, vault_path.to_str().unwrap()],
    );
    assert!(ok, "sync A2 failed: {out}");
    assert!(
        out.contains("sync complete: 0 new, 0 unchanged, 1 modified"),
        "revert to A must be Modified because predecessor differs: {out}"
    );
    assert_eq!(
        episode_count(store.path()),
        3,
        "A→B→A must create three distinct events"
    );

    let occ_a2 = oxibrain_core::occurrence_id(&source_id, locator, Some(&occ_b), &hash_a);

    // Verify via Brain::locator_states: exactly one entry whose
    // latest_occurrence_id matches the deterministic occ_a2.
    let states = runtime.block_on(async {
        let brain = oxibrain::Brain::open_ro(oxibrain::BrainConfig::at(store.path()))
            .await
            .expect("open_ro");
        brain
            .locator_states(&space_id, &source_id)
            .await
            .expect("locator_states")
    });
    assert_eq!(
        states.len(),
        1,
        "exactly one locator state expected: {states:?}"
    );
    let state = states.get(locator).expect("note.md state");
    assert_eq!(
        state.latest_occurrence_id, occ_a2,
        "final occurrence must equal H(source_id, locator, Some(occ_b), hash(A))"
    );
    // Chain depth proof: occ_a2 != occ_a1. If the chain had been broken and
    // the ingest had collapsed to the original occurrence, the latest id
    // would equal occ_a1 (and dedup would have left episode_count at 2).
    assert_ne!(
        state.latest_occurrence_id, occ_a1,
        "occ_a2 must differ from occ_a1 — chain is broken"
    );
}
