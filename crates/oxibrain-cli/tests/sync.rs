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
