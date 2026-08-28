//! End-to-end tests for the agent op surface (spec agent-first-cli-v1).
//!
//! The CLI is a first-class agent surface: an op call must print one JSON
//! envelope, exit with the envelope's code, and carry content excerpts in
//! `data` — not opaque target ids. This replaces the retired `ask`-verb
//! test with the equivalent contract on the op transport.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::tempdir;

fn run_oxibrain(store: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .arg("--dir")
        .arg(store)
        .args(args)
        .output()
        .expect("spawn oxibrain");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_oxibrain_stdin(store: &std::path::Path, args: &[&str], stdin_data: &str) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .arg("--dir")
        .arg(store)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn oxibrain");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin_data.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn init_store(store: &std::path::Path) {
    let (ok, out, err) = run_oxibrain(store, &["admin", "init"]);
    assert!(ok, "admin init failed: {out} {err}");
}

#[test]
fn ingest_op_envelope_via_stdin_payload() {
    let store = tempdir().expect("store dir");
    init_store(store.path());

    // Prose bodies ride the stdin payload path — never argv (spec §2).
    let content =
        "# Rule\n\nDeployment convention: always use squash merge with conventional commit titles.";
    let payload = format!(
        "{{\"space\":\"personal\",\"source_path\":\"test/note.md\",\"content\":{}}}",
        serde_json::to_string(content).unwrap()
    );
    let (ok, out) = run_oxibrain_stdin(store.path(), &["ingest"], &payload);
    assert!(ok, "ingest failed: {out}");
    let env: serde_json::Value = serde_json::from_str(&out).expect("envelope JSON");
    assert_eq!(env["ok"], serde_json::json!(true), "envelope: {out}");
    assert_eq!(env["op"], "ingest");
    assert_eq!(env["space"], "personal");
    // Without a sampling session the CLI transport degrades honestly: the
    // episode is captured, extraction is skipped (spec §8), and the
    // response text says so.
    let text = env["data"].as_str().unwrap_or_default();
    assert!(
        text.contains("Ingested as episode"),
        "expected capture text, got: {out}"
    );
}

#[test]
fn search_op_returns_envelope_with_entity_hit() {
    let store = tempdir().expect("store dir");
    init_store(store.path());

    // Deterministic seed: declare needs no model (memory-plane hits are
    // extraction products, so an unextracted ingest would not hit).
    let decl = r#"{"op":"add_statement","subject":{"surface":"Alice","type":"Person"},"predicate":"employed_by","object":{"kind":"entity","surface":"Acme Corp","type":"Organization"},"polarity":"affirm","valid_from":1000,"valid_to":4102444800000}"#;
    let payload = format!(
        "{{\"space\":\"personal\",\"declaration_json\":{}}}",
        serde_json::to_string(decl).unwrap()
    );
    let (ok, out) = run_oxibrain_stdin(store.path(), &["declare"], &payload);
    assert!(ok, "declare failed: {out}");

    let (ok, out, _) = run_oxibrain(
        store.path(),
        &[
            "search",
            "--json",
            r#"{ "space": "personal", "query": "Acme" }"#,
        ],
    );
    assert!(ok, "search failed: {out}");
    let env: serde_json::Value = serde_json::from_str(&out).expect("envelope JSON");
    assert_eq!(env["ok"], serde_json::json!(true), "envelope: {out}");
    assert_eq!(env["op"], "search");
    // v2.13 P5: read-op tool results carry `{data, meta}`; the op envelope's
    // `data` IS that result, so the search DTO sits at data.data.
    let hits = env["data"]["data"]["memory"]
        .as_array()
        .expect("memory hits");
    assert!(!hits.is_empty(), "expected at least one hit: {out}");
}

#[test]
fn unknown_op_is_invalid_input_with_op_list() {
    let store = tempdir().expect("store dir");
    init_store(store.path());
    let (ok, out, _) = run_oxibrain(store.path(), &["not_an_op", "--json", "{}"]);
    assert!(!ok);
    let env: serde_json::Value = serde_json::from_str(&out).expect("envelope JSON");
    assert_eq!(env["ok"], serde_json::json!(false));
    assert_eq!(env["error"]["code"], "invalid_input");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("search"),
        "error must list ops: {out}"
    );
}

#[test]
fn unknown_space_fails_fast_with_hint() {
    let store = tempdir().expect("store dir");
    init_store(store.path());
    let (ok, out, _) = run_oxibrain(
        store.path(),
        &["contradictions", "--json", r#"{ "space": "ghost" }"#],
    );
    assert!(!ok);
    let env: serde_json::Value = serde_json::from_str(&out).expect("envelope JSON");
    assert_eq!(env["error"]["code"], "invalid_input");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("space add"),
        "error must carry the space add hint: {out}"
    );
}

#[test]
fn describe_lists_spaces_as_enum_source() {
    let store = tempdir().expect("store dir");
    init_store(store.path());
    let (ok, out, _) = run_oxibrain(store.path(), &["describe"]);
    assert!(ok, "describe failed: {out}");
    let env: serde_json::Value = serde_json::from_str(&out).expect("envelope JSON");
    let spaces = env["data"]["spaces"].as_array().expect("spaces");
    assert!(
        spaces.iter().any(|s| s["name"] == "personal"),
        "personal must be listed: {out}"
    );
}
