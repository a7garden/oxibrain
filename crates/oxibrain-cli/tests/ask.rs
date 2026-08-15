//! End-to-end test for `oxibrain ask` output. The CLI is a first-class agent
//! surface: an agent that asks a question must see the matched content, not
//! only opaque target ids.

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

#[test]
fn ask_prints_content_excerpt_for_episode_hits() {
    let store = tempdir().expect("store dir");
    let note = tempdir().expect("note dir");
    let file = note.path().join("rule.md");
    fs::write(
        &file,
        "# Rule\n\nDeployment convention: always use squash merge with conventional commit titles.",
    )
    .expect("write note");

    let (ok, out) = run_oxibrain(store.path(), &["ingest", file.to_str().unwrap()]);
    assert!(ok, "ingest failed: {out}");

    let (ok, out) = run_oxibrain(
        store.path(),
        &["ask", "squash merge", "--space", "personal"],
    );
    assert!(ok, "ask failed: {out}");
    assert!(out.contains("hits: 1"), "expected one hit: {out}");
    assert!(
        out.contains("squash merge with conventional commit"),
        "episode hits must print a content excerpt: {out}"
    );
}
