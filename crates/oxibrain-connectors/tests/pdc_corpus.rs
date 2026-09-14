//! PDC conformance corpus runner plus oxibrain-specific fixture-root tests.
//!
//! The corpus is vendored at `tests/fixtures/pdc-corpus/` (pin recorded in
//! `PIN.md`); this file never reads the sibling contract checkout. Every case
//! runs against the pure parse/classify API inside an isolated temporary
//! vault, and asserts the Reader guarantees: deterministic results, source
//! bytes never mutated.
//!
//! Expectation mapping (corpus revision 3):
//! - `file` cases decide by `expect`: `valid` ⇒ Ok + content assertions,
//!   `unsafe_content` ⇒ Ok + non-empty `unsafe_constructs`, `legacy_html` ⇒
//!   classification only, anything else ⇒ `Err` whose `PdcDiagnosticCode`
//!   spelling equals the corpus string.
//! - `operation` error cases (`prefix-source-bytes`, `pad-body-to-total-bytes`,
//!   `replace-body-with-nested-block-quotes`) synthesize the described bytes
//!   in-test and assert the diagnostic.
//! - Writer-only operations (`no-op-round-trip`, `metadata-patch`,
//!   `external-change-before-save`) have no Writer here — oxibrain never
//!   writes a user document — so their Reader equivalents are asserted.
//! - `set` cases parse all members in one vault; vault-level conflict
//!   resolution is the facade's job and is asserted there, not here.

use oxibrain_connectors::pdc::{
    BodyProfile, HtmlClassification, PDC_CORPUS_REVISION, PdcDiagnostic, PdcDocument,
    classify_html_transport, parse_djot_document, parse_html_document,
};
use oxibrain_connectors::{RootEntry, scan_root};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Vendored corpus root; the upstream pin is recorded in `PIN.md`.
const CORPUS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pdc-corpus");

// --- corpus plumbing ---------------------------------------------------------

fn corpus_json() -> Value {
    let raw = fs::read_to_string(Path::new(CORPUS_ROOT).join("corpus.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn fixture_bytes(corpus_rel: &str) -> Vec<u8> {
    fs::read(Path::new(CORPUS_ROOT).join(corpus_rel)).unwrap()
}

/// Copy a corpus fixture (by its corpus-relative path) into `vault`, keeping
/// the relative layout so set members stay together.
fn install_fixture(vault: &Path, corpus_rel: &str) -> PathBuf {
    let dest = vault.join(corpus_rel);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, fixture_bytes(corpus_rel)).unwrap();
    dest
}

/// Write synthesized bytes under `name` in a fresh vault.
fn synthesize(vault: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let dest = vault.join(name);
    fs::write(&dest, bytes).unwrap();
    dest
}

/// Parse with the profile implied by the extension. `file_stem` feeds the
/// display-title fallback exactly as the scan/facade layer would pass it.
fn parse_document(path: &Path, bytes: &[u8]) -> Result<PdcDocument, PdcDiagnostic> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match path.extension().and_then(|e| e.to_str()) {
        Some("djot") => parse_djot_document(stem, bytes),
        Some("html") => parse_html_document(stem, bytes),
        other => panic!("no parse entry point for extension {other:?}"),
    }
}

/// Single-expectation helper producing a readable failure message.
fn expect(cond: bool, what: &str) -> Result<(), String> {
    if cond { Ok(()) } else { Err(what.to_string()) }
}

/// The source bytes on disk are byte-identical after the read attempt
/// (PDC-1.0 §10.1: reads never write).
fn assert_bytes_unchanged(path: &Path, before: &[u8]) -> Result<(), String> {
    let after = fs::read(path).map_err(|e| format!("re-read {}: {e}", path.display()))?;
    expect(
        before == after,
        &format!("{}: source bytes changed during a read", path.display()),
    )
}

/// Reader inertness for documents that must parse: two parses agree, and the
/// source file is untouched.
fn assert_reader_is_inert(path: &Path) -> Result<(), String> {
    let before = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let first =
        parse_document(path, &before).map_err(|d| format!("parse {}: {d}", path.display()))?;
    let second =
        parse_document(path, &before).map_err(|d| format!("re-parse {}: {d}", path.display()))?;
    expect(
        first == second,
        &format!("{}: parsing is not deterministic", path.display()),
    )?;
    assert_bytes_unchanged(path, &before)
}

/// The operation failed with exactly the corpus-pinned diagnostic code.
fn expect_code(diag: &PdcDiagnostic, case: &Value) -> Result<(), String> {
    let want = case["expect"].as_str().unwrap_or_default();
    expect(
        diag.code.as_str() == want,
        &format!("expected diagnostic {want:?}, got {diag}"),
    )
}

// --- corpus pin --------------------------------------------------------------

#[test]
fn corpus_revision_pin_matches_implementation() {
    let corpus = corpus_json();
    assert_eq!(
        corpus["format"].as_str(),
        Some("pdc-document-conformance/1")
    );
    assert_eq!(
        corpus["revision"].as_u64(),
        Some(3),
        "vendored corpus moved — refresh per tests/fixtures/pdc-corpus/PIN.md"
    );
    assert_eq!(PDC_CORPUS_REVISION, 3);
    let cases = corpus["cases"].as_array().expect("cases array");
    assert_eq!(
        cases.len(),
        30,
        "corpus case count changed — refresh per PIN.md and re-check the runner"
    );
}

// --- corpus runner -----------------------------------------------------------

#[test]
fn corpus_conformance() {
    let cases = corpus_json()["cases"]
        .as_array()
        .expect("cases array")
        .clone();
    assert!(!cases.is_empty(), "vendored corpus is empty");
    let mut failures = Vec::new();
    for case in &cases {
        let id = case["id"].as_str().unwrap_or("(no id)").to_string();
        if let Err(msg) = run_case(case) {
            failures.push(format!("{id}: {msg}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} corpus cases failed:\n  {}",
        failures.len(),
        cases.len(),
        failures.join("\n  ")
    );
}

fn run_case(case: &Value) -> Result<(), String> {
    match case["kind"].as_str().unwrap_or_default() {
        "file" => run_file_case(case),
        "operation" => run_operation_case(case),
        "set" => run_set_case(case),
        other => Err(format!("unknown case kind {other:?}")),
    }
}

fn run_file_case(case: &Value) -> Result<(), String> {
    let id = case["id"].as_str().unwrap_or_default();
    let rel = case["path"].as_str().expect("file case has path");
    let expect_kind = case["expect"].as_str().expect("case has expect");
    let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let dest = install_fixture(vault.path(), rel);
    let source = fs::read(&dest).map_err(|e| format!("read vendored copy: {e}"))?;

    if expect_kind == "legacy_html" {
        // Legacy HTML never enters the PDC parse path; classification routes
        // it to the legacy decoder instead.
        expect(
            classify_html_transport(&source) == HtmlClassification::Legacy,
            &format!("{rel} must classify as legacy HTML"),
        )?;
    } else {
        let parsed = parse_document(&dest, &source);
        match expect_kind {
            "valid" => {
                let doc = parsed.map_err(|d| format!("expected a valid document, got {d}"))?;
                assert_valid_document(id, &doc)?;
            }
            "unsafe_content" => {
                let doc = parsed.map_err(|d| format!("unsafe content still parses, got {d}"))?;
                expect(
                    !doc.body.unsafe_constructs.is_empty(),
                    "expected non-empty body.unsafe_constructs",
                )?;
            }
            code => {
                let diag = parsed.expect_err("expected a diagnostic");
                expect(
                    diag.code.as_str() == code,
                    &format!("expected diagnostic {code:?}, got {diag}"),
                )?;
            }
        }
    }

    assert_bytes_unchanged(&dest, &source)
}

/// Content assertions for the four valid fixtures. A future corpus pin that
/// adds another `valid` fixture must extend this match — parsing without
/// content assertions is not a conformance test.
fn assert_valid_document(case_id: &str, doc: &PdcDocument) -> Result<(), String> {
    let m = &doc.metadata;
    let b = &doc.body;
    match case_id {
        "valid-minimal" => {
            expect(
                m.document_uuid == "018f47c6-4a77-7c52-9db8-0e5f9bcb17db",
                "uuid",
            )?;
            expect(m.body == BodyProfile::Djot, "body profile")?;
            expect(m.title == "Minimal document", "envelope title")?;
            expect(doc.display_title == "Minimal document", "display title")?;
            expect(m.created == "2026-09-13T12:34:56.789Z", "created")?;
            expect(m.updated == "2026-09-13T12:34:56.789Z", "updated")?;
            expect(m.tags.is_empty() && m.aliases.is_empty(), "no tags/aliases")?;
            expect(
                !m.favorite && !m.deleted && m.deleted_at.is_none(),
                "not favorite/deleted",
            )?;
            expect(m.profile.is_none() && m.lang.is_none(), "no profile/lang")?;
            expect(
                b.block_ids.is_empty()
                    && b.document_links.is_empty()
                    && b.tasks.is_empty()
                    && b.asset_refs.is_empty()
                    && b.unsafe_constructs.is_empty(),
                "minimal body carries no artifacts",
            )?;
            expect(b.text.contains("This is canonical Djot."), "body text")?;
            assert_text_clean(doc, true)
        }
        "valid-semantics" => {
            expect(
                m.document_uuid == "018f47c6-7ae7-7aa1-8ed8-8e3df921e7c4",
                "uuid",
            )?;
            expect(m.body == BodyProfile::Djot, "body profile")?;
            expect(m.title == "Semantic document", "envelope title")?;
            expect(doc.display_title == "Semantic document", "display title")?;
            expect(m.profile.as_deref() == Some("note"), "profile")?;
            expect(m.lang.as_deref() == Some("en"), "lang")?;
            expect(m.tags == ["standard", "interop"], "tags")?;
            expect(m.aliases == ["Fixture"], "aliases")?;
            expect(
                m.favorite && !m.deleted && m.deleted_at.is_none(),
                "favorite, not deleted",
            )?;
            expect(m.created == "2026-09-13T12:35:00.000Z", "created")?;
            expect(m.updated == "2026-09-13T12:36:00.000Z", "updated")?;
            expect(
                b.block_ids == ["b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32"],
                "block ids",
            )?;
            expect(b.document_links.len() == 2, "two links")?;
            let target = "018f47c6-4a77-7c52-9db8-0e5f9bcb17db";
            expect(
                b.document_links
                    .iter()
                    .all(|l| l.uuid == target && l.block.is_none()),
                "both links target the minimal document",
            )?;
            expect(
                !b.document_links[0].embed && b.document_links[1].embed,
                "second link embeds",
            )?;
            expect(b.tasks.len() == 2, "two tasks")?;
            expect(
                !b.tasks[0].completed && b.tasks[0].id.is_none(),
                "open task, no id",
            )?;
            expect(b.tasks[0].text.contains("Open task"), "task text")?;
            expect(b.tasks[1].completed, "second task completed")?;
            expect(b.tasks[1].text.contains("Completed task"), "task text")?;
            expect(
                b.asset_refs.is_empty() && b.unsafe_constructs.is_empty(),
                "no assets/unsafe",
            )?;
            expect(
                b.fallback_title.as_deref() == Some("Semantic document"),
                "title fallback",
            )?;
            expect(b.text.contains("Stable section"), "section text")?;
            // The pdc-query block is opaque but shown: its content is body text.
            expect(
                b.text.contains("tag = \"standard\""),
                "query block content is body text",
            )?;
            assert_text_clean(doc, true)
        }
        "valid-html-minimal" => {
            expect(
                m.document_uuid == "018f47c6-4a77-7c52-9db8-0e5f9bcb1701",
                "uuid",
            )?;
            expect(m.body == BodyProfile::Html, "body profile")?;
            expect(m.title == "Minimal HTML", "envelope title")?;
            expect(doc.display_title == "Minimal HTML", "display title")?;
            expect(m.created == "2026-09-13T12:34:56.789Z", "created")?;
            expect(m.updated == "2026-09-13T12:34:56.789Z", "updated")?;
            expect(m.tags.is_empty() && m.aliases.is_empty(), "no tags/aliases")?;
            expect(
                !m.favorite && !m.deleted && m.deleted_at.is_none(),
                "not favorite/deleted",
            )?;
            expect(
                m.profile.is_none() && m.lang.is_none(),
                "lang comes from the envelope",
            )?;
            expect(
                b.block_ids.is_empty()
                    && b.document_links.is_empty()
                    && b.tasks.is_empty()
                    && b.asset_refs.is_empty()
                    && b.unsafe_constructs.is_empty(),
                "minimal body carries no artifacts",
            )?;
            expect(b.text.contains("Minimal HTML"), "body text")?;
            assert_text_clean(doc, false)
        }
        "valid-html-semantics" => {
            expect(
                m.document_uuid == "018f47c6-4a77-7c52-9db8-0e5f9bcb1702",
                "uuid",
            )?;
            expect(m.body == BodyProfile::Html, "body profile")?;
            expect(m.title == "HTML semantics", "envelope title")?;
            expect(doc.display_title == "HTML semantics", "display title")?;
            expect(m.profile.as_deref() == Some("note"), "profile")?;
            expect(m.lang.is_none(), "lang comes from the envelope")?;
            expect(m.tags == ["portable", "html"], "tags")?;
            expect(m.aliases.is_empty(), "no aliases")?;
            expect(
                m.favorite && !m.deleted && m.deleted_at.is_none(),
                "favorite, not deleted",
            )?;
            expect(m.created == "2026-09-13T12:34:56.789Z", "created")?;
            expect(m.updated == "2026-09-13T12:35:56.789Z", "updated")?;
            expect(
                b.block_ids
                    .contains(&"b-018f47c6-7dbe-7a14-9f67-6f89a5e3c170".to_string()),
                "h1 id is a stable block target",
            )?;
            expect(b.document_links.len() == 1, "one link")?;
            expect(
                b.document_links[0].uuid == "018f47c6-4a77-7c52-9db8-0e5f9bcb17db"
                    && !b.document_links[0].embed,
                "link target",
            )?;
            expect(b.tasks.len() == 1, "one task")?;
            expect(!b.tasks[0].completed, "task open")?;
            expect(
                b.tasks[0].id.as_deref() == Some("b-018f47c6-c718-728c-9d91-b2bc70081170"),
                "task li id",
            )?;
            expect(b.tasks[0].text.contains("Open task"), "task text")?;
            expect(
                b.asset_refs
                    == ["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"],
                "asset digest ref",
            )?;
            expect(
                b.unsafe_constructs.is_empty(),
                "inline <style> is authored layout, not unsafe",
            )?;
            expect(
                b.fallback_title.as_deref() == Some("HTML semantics"),
                "title fallback",
            )?;
            expect(
                b.text.contains("Related document"),
                "link label stays readable",
            )?;
            assert_text_clean(doc, false)
        }
        other => Err(format!(
            "valid case {other:?} has no content assertions — extend assert_valid_document"
        )),
    }
}

/// Envelope/transport spellings and markup must never leak into indexable
/// body text.
fn assert_text_clean(doc: &PdcDocument, djot: bool) -> Result<(), String> {
    let text = &doc.body.text;
    for marker in ["pdc-document/1", "format:", "created:", "updated:", "---"] {
        expect(
            !text.contains(marker),
            &format!("envelope marker {marker:?} leaked into text"),
        )?;
    }
    expect(
        !text.contains("pdc://"),
        "canonical URIs must not leak into text",
    )?;
    if djot {
        expect(
            !text.contains("{#"),
            "djot block-id attributes must not leak",
        )?;
        expect(
            !text.contains("{.pdc-embed}"),
            "djot embed attribute must not leak",
        )?;
    } else {
        expect(!text.contains('<'), "html tags must not leak")?;
    }
    Ok(())
}

fn run_operation_case(case: &Value) -> Result<(), String> {
    let op = case["operation"].as_str().unwrap_or_default();
    let input = case["input"].as_str().expect("operation case has input");
    let name = Path::new(input)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    match op {
        // Prepend the hex bytes (a UTF-8 BOM): the transport must reject it
        // and never strip it.
        "prefix-source-bytes" => {
            let hex = case["prefixHex"].as_str().expect("prefixHex");
            let mut bytes = decode_hex(hex);
            bytes.extend_from_slice(&fixture_bytes(input));
            let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
            let dest = synthesize(vault.path(), name, &bytes);
            let diag = parse_document(&dest, &bytes).expect_err("BOM must be rejected");
            expect_code(&diag, case)?;
            assert_bytes_unchanged(&dest, &bytes)
        }
        // Pad the body so the total file exceeds the 4 MiB document cap by
        // exactly one byte.
        "pad-body-to-total-bytes" => {
            let total = case["totalBytes"].as_u64().expect("totalBytes") as usize;
            let mut bytes = fixture_bytes(input);
            assert!(
                total > bytes.len(),
                "fixture already exceeds the target size"
            );
            bytes.resize(total, b'\n');
            let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
            let dest = synthesize(vault.path(), name, &bytes);
            let diag = parse_document(&dest, &bytes).expect_err("over-cap must be rejected");
            expect_code(&diag, case)?;
            assert_bytes_unchanged(&dest, &bytes)
        }
        // Replace the body with `depth` nested block-quote levels.
        "replace-body-with-nested-block-quotes" => {
            let depth = case["containerDepth"].as_u64().expect("containerDepth") as usize;
            let text = String::from_utf8(fixture_bytes(input)).unwrap();
            let bytes = djot_with_body(&text, &nested_quotes_body(depth)).into_bytes();
            let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
            let dest = synthesize(vault.path(), name, &bytes);
            let diag = parse_document(&dest, &bytes).expect_err("over-depth must be rejected");
            expect_code(&diag, case)?;
            assert_bytes_unchanged(&dest, &bytes)
        }
        // Writer-only operations below: oxibrain is a Reader and never
        // writes, so the pinned preservation laws are asserted through their
        // Reader equivalents.
        "no-op-round-trip" => {
            let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
            let dest = install_fixture(vault.path(), input);
            assert_reader_is_inert(&dest)
        }
        "metadata-patch" => {
            let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
            let dest = install_fixture(vault.path(), input);
            let before = fs::read(&dest).unwrap();
            let input_doc =
                parse_document(&dest, &before).map_err(|d| format!("input must parse: {d}"))?;
            // The Writer-side patch would produce the `expected` bytes; the
            // Reader-side fact is that those bytes differ from the input in
            // exactly `updated` + `title` while the whole body is preserved.
            let expected_rel = case["expected"]
                .as_str()
                .expect("metadata-patch names expected");
            let expected_bytes = fixture_bytes(expected_rel);
            let expected_dest = synthesize(
                vault.path(),
                Path::new(expected_rel)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("expected"),
                &expected_bytes,
            );
            let expected_doc = parse_document(&expected_dest, &expected_bytes)
                .map_err(|d| format!("expected file must parse: {d}"))?;
            expect(
                input_doc.body == expected_doc.body,
                "metadata patch must preserve the body",
            )?;
            let mut reverted = expected_doc.metadata.clone();
            reverted.updated = input_doc.metadata.updated.clone();
            reverted.title = input_doc.metadata.title.clone();
            expect(
                reverted == input_doc.metadata,
                "metadata patch touches only updated + title",
            )?;
            assert_reader_is_inert(&dest)
        }
        "external-change-before-save" => {
            let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
            let original = install_fixture(vault.path(), input);
            let external_rel = case["external"].as_str().expect("external file");
            let external = install_fixture(vault.path(), external_rel);
            // Both snapshots stay independently readable and untouched; the
            // conflict itself is a Writer-side decision.
            assert_reader_is_inert(&original)?;
            assert_reader_is_inert(&external)
        }
        other => Err(format!("unknown operation {other:?}")),
    }
}

fn run_set_case(case: &Value) -> Result<(), String> {
    let paths: Vec<&str> = case["paths"]
        .as_array()
        .expect("set case has paths")
        .iter()
        .map(|p| p.as_str().expect("path string"))
        .collect();
    let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let mut uuids = Vec::new();
    for rel in paths {
        let dest = install_fixture(vault.path(), rel);
        let bytes = fs::read(&dest).unwrap();
        let doc = parse_document(&dest, &bytes)
            .map_err(|d| format!("{rel} must parse in the set: {d}"))?;
        uuids.push(doc.metadata.document_uuid);
        assert_bytes_unchanged(&dest, &bytes)?;
    }
    // Reader-level fact pinned by the corpus: every member carries the same
    // canonical document id. Judging that a vault-level duplicate (and
    // dropping conflicting upserts) is the facade's job, asserted there.
    let unique: HashSet<&String> = uuids.iter().collect();
    expect(
        unique.len() == 1,
        &format!("set members must share one document UUID, got {uuids:?}"),
    )
}

/// Replace the body region of a djot fixture, keeping the `---` transport
/// envelope byte-identical.
fn djot_with_body(text: &str, body: &str) -> String {
    let mut envelope = String::new();
    let mut closers = 0;
    for line in text.lines() {
        envelope.push_str(line);
        envelope.push('\n');
        if line == "---" {
            closers += 1;
            if closers == 2 {
                break;
            }
        }
    }
    assert_eq!(closers, 2, "fixture lacks a closed djot envelope");
    envelope.push_str(body);
    envelope
}

/// `depth` nested block-quote levels — one line per level.
fn nested_quotes_body(depth: usize) -> String {
    let mut body = String::new();
    for level in 1..=depth {
        body.push_str(&">".repeat(level));
        body.push('\n');
    }
    body
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "odd-length hex: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
        .collect()
}

// --- vendored-fixture integrity ---------------------------------------------

/// Walk every vendored fixture, parse it (any outcome), and assert the source
/// bytes on disk are identical afterwards. This is the Reader-side form of
/// the corpus's no-op preservation law: indexing must never mutate a vault.
#[test]
fn parsing_never_mutates_any_vendored_fixture() {
    let fixtures = Path::new(CORPUS_ROOT).join("fixtures");
    let mut stack = vec![fixtures];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if !matches!(ext, "djot" | "html") {
                continue;
            }
            let before = fs::read(&path).unwrap();
            let _outcome = parse_document(&path, &before); // valid, invalid, unsafe: any outcome
            let after = fs::read(&path).unwrap();
            assert_eq!(before, after, "source mutated: {}", path.display());
            checked += 1;
        }
    }
    assert!(
        checked >= 32,
        "walked only {checked} fixtures — expected the full vendored set"
    );
}

// --- fixture-root behavior (scan_root) ---------------------------------------

/// A test vault root that accepts the document extensions under test.
fn vault_root(dir: &Path) -> RootEntry {
    RootEntry {
        alias: "corpus-vault".into(),
        path: dir.to_path_buf(),
        space: "test".into(),
        include: vec![
            "**/*.djot".into(),
            "**/*.html".into(),
            "**/*.md".into(),
            "**/*.txt".into(),
        ],
        exclude: vec![],
        max_file_bytes: 8 * 1024 * 1024,
    }
}

fn write_rel(dir: &Path, rel: &str, body: &[u8]) {
    let full = dir.join(rel);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, body).unwrap();
}

#[test]
fn scan_accepts_djot_documents() {
    let vault = tempdir().unwrap();
    fs::write(
        vault.path().join("note.djot"),
        fixture_bytes("fixtures/valid/minimal.djot"),
    )
    .unwrap();
    fs::write(vault.path().join("readme.md"), "# legacy\n").unwrap();

    let result = scan_root(&vault_root(vault.path())).unwrap();
    let djot = result
        .observations
        .iter()
        .find(|o| o.locator.ends_with("note.djot"))
        .expect(".djot files must surface as observations");
    assert!(djot.bytes > 0);
    assert!(
        result
            .observations
            .iter()
            .any(|o| o.locator.ends_with("readme.md"))
    );
    assert!(
        result.skipped.is_empty(),
        "unexpected skips: {:?}",
        result.skipped
    );
}

#[test]
fn scan_prunes_hidden_directories_and_pdc_state() {
    let vault = tempdir().unwrap();
    write_rel(
        vault.path(),
        "note.djot",
        &fixture_bytes("fixtures/valid/minimal.djot"),
    );
    write_rel(vault.path(), ".git/config", b"[core]\n");
    write_rel(
        vault.path(),
        ".pdc/assets/sha256/01/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        b"asset bytes",
    );
    write_rel(vault.path(), ".hidden/secret.djot", b"secret\n");
    write_rel(vault.path(), ".trash/note.djot", b"stale\n");

    let result = scan_root(&vault_root(vault.path())).unwrap();
    assert_eq!(
        result.observations.len(),
        1,
        "only the visible document is observed: {:?}",
        result.observations
    );
    assert!(result.observations[0].locator.ends_with("note.djot"));
    for hidden in [".git", ".pdc", ".hidden", ".trash"] {
        for observed in result.observations.iter().map(|o| o.locator.as_str()) {
            assert!(
                !observed.contains(hidden),
                "hidden path observed: {observed}"
            );
        }
        for skipped in result.skipped.iter().map(|s| s.locator.as_str()) {
            assert!(!skipped.contains(hidden), "hidden path in skips: {skipped}");
        }
    }
}

#[test]
fn scan_never_follows_symlinks() {
    let vault = tempdir().unwrap();
    let outside = tempdir().unwrap();
    // Real document plus a file symlink to it, both inside the vault.
    fs::write(
        vault.path().join("real.djot"),
        fixture_bytes("fixtures/valid/minimal.djot"),
    )
    .unwrap();
    symlink("real.djot", vault.path().join("alias.djot")).unwrap();
    // File symlink pointing outside the vault.
    fs::write(outside.path().join("outside.djot"), b"outside\n").unwrap();
    symlink(
        outside.path().join("outside.djot"),
        vault.path().join("leak.djot"),
    )
    .unwrap();
    // Directory symlink inside the vault plus one pointing outside.
    fs::create_dir(vault.path().join("sub")).unwrap();
    fs::write(
        vault.path().join("sub/inner.djot"),
        fixture_bytes("fixtures/valid/semantics.djot"),
    )
    .unwrap();
    symlink("sub", vault.path().join("dirlink")).unwrap();
    symlink(outside.path(), vault.path().join("outside-dir")).unwrap();

    let result = scan_root(&vault_root(vault.path())).unwrap();
    let locators: Vec<&str> = result
        .observations
        .iter()
        .map(|o| o.locator.as_str())
        .collect();
    for name in ["alias.djot", "leak.djot", "dirlink", "outside-dir"] {
        assert!(
            !locators.iter().any(|l| l.contains(name)),
            "symlink {name} must never be followed, saw {locators:?}"
        );
    }
    // The symlinked targets stay visible exactly once, via their real path.
    assert_eq!(
        locators.iter().filter(|l| l.ends_with("real.djot")).count(),
        1
    );
    assert_eq!(
        locators
            .iter()
            .filter(|l| l.ends_with("inner.djot"))
            .count(),
        1
    );
    assert!(locators.iter().any(|l| l.ends_with("real.djot")));
    assert!(locators.iter().any(|l| l.ends_with("inner.djot")));
}

#[test]
fn scan_skips_over_cap_but_over_4mib_still_reaches_parse() {
    // Above the per-root cap: the walker reports the file as skipped.
    let capped = tempdir().unwrap();
    fs::write(
        capped.path().join("big.djot"),
        fixture_bytes("fixtures/valid/minimal.djot"),
    )
    .unwrap();
    let mut entry = vault_root(capped.path());
    entry.max_file_bytes = 64;
    let result = scan_root(&entry).unwrap();
    assert!(
        result.observations.is_empty(),
        "nothing fits the 64-byte cap"
    );
    let skip = result
        .skipped
        .iter()
        .find(|s| s.locator.ends_with("big.djot"))
        .expect("oversize file must be reported, not dropped");
    assert!(skip.reason.contains("oversize"), "reason: {}", skip.reason);

    // Within the per-root cap but over the 4 MiB document cap: the scan
    // surfaces the file and the PARSER reports document_too_large.
    let vault = tempdir().unwrap();
    let mut oversized = fixture_bytes("fixtures/valid/minimal.djot");
    oversized.resize(4 * 1024 * 1024 + 1, b'\n');
    fs::write(vault.path().join("huge.djot"), &oversized).unwrap();
    let result = scan_root(&vault_root(vault.path())).unwrap();
    assert!(
        result
            .observations
            .iter()
            .any(|o| o.locator.ends_with("huge.djot")),
        "within the root cap the walker must surface the file"
    );
    let dest = vault.path().join("huge.djot");
    let diag = parse_document(&dest, &oversized).expect_err("over 4 MiB must fail the parse");
    assert_eq!(diag.code.as_str(), "document_too_large");
}

/// Classification-only check kept next to the corpus suite (filterable via
/// `cargo test -p oxibrain-connectors --test pdc_corpus legacy_html`).
#[test]
fn legacy_html_is_classified_before_any_parse() {
    assert_eq!(
        classify_html_transport(&fixture_bytes("fixtures/legacy/unmarked.html")),
        HtmlClassification::Legacy
    );
    assert_eq!(
        classify_html_transport(&fixture_bytes("fixtures/valid/minimal.html")),
        HtmlClassification::Pdc
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// pdc-document-conformance/2 runner (vendored at tests/fixtures/pdc2-corpus,
// pin: commit 0ee51ea, tag v2.0.0-draft.2, revision 2 — see its PIN.md).
//
// Same discipline as the v1 runner above: every `file` case runs against the
// pure parse/classify API in an isolated vault, valid fixtures get content
// assertions, and the source bytes are asserted byte-identical afterwards.
// Writer-only operations run their Reader equivalents (reads never write).
// ═══════════════════════════════════════════════════════════════════════════

use oxibrain_connectors::pdc::{
    MarkdownClassification, PDC_QUERY_CONTRACT, PDC2_CORPUS_FORMAT, PDC2_CORPUS_REVISION,
    parse_markdown_document, sniff_markdown, validate_base_query,
};

const CORPUS2_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pdc2-corpus");

fn corpus2_json() -> Value {
    serde_json::from_str(&fs::read_to_string(Path::new(CORPUS2_ROOT).join("corpus.json")).unwrap())
        .unwrap()
}

fn fixture2_bytes(corpus_rel: &str) -> Vec<u8> {
    fs::read(Path::new(CORPUS2_ROOT).join(corpus_rel)).unwrap()
}

fn install_fixture2(vault: &Path, corpus_rel: &str) -> PathBuf {
    let dest = vault.join(corpus_rel);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, fixture2_bytes(corpus_rel)).unwrap();
    dest
}

/// Parse (or query-validate) with the profile implied by the extension.
fn parse_document2(path: &Path, bytes: &[u8]) -> Result<PdcDocument, PdcDiagnostic> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match path.extension().and_then(|e| e.to_str()) {
        Some("md") => parse_markdown_document(stem, bytes),
        Some("html") => parse_html_document(stem, bytes),
        Some("djot") => parse_djot_document(stem, bytes),
        other => panic!("no v2 parse entry point for extension {other:?}"),
    }
}

#[test]
fn corpus2_revision_pin_matches_implementation() {
    let corpus = corpus2_json();
    assert_eq!(corpus["format"].as_str(), Some(PDC2_CORPUS_FORMAT));
    assert_eq!(
        corpus["revision"].as_u64(),
        Some(PDC2_CORPUS_REVISION as u64),
        "vendored corpus moved — refresh per tests/fixtures/pdc2-corpus/PIN.md"
    );
    assert_eq!(corpus["queryContract"].as_str(), Some(PDC_QUERY_CONTRACT));
    assert_eq!(PDC2_CORPUS_REVISION, 2);
}

#[test]
fn corpus2_conformance() {
    let cases = corpus2_json()["cases"].as_array().expect("cases").clone();
    assert!(!cases.is_empty(), "vendored v2 corpus is empty");
    let mut failures = Vec::new();
    for case in &cases {
        let id = case["id"].as_str().unwrap_or("(no id)").to_string();
        if let Err(msg) = run_case2(case) {
            failures.push(format!("{id}: {msg}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} v2 corpus cases failed:\n  {}",
        failures.len(),
        cases.len(),
        failures.join("\n  ")
    );
}

fn run_case2(case: &Value) -> Result<(), String> {
    match case["kind"].as_str().unwrap_or_default() {
        "file" => run_file_case2(case),
        "operation" => run_operation_case2(case),
        "set" => run_set_case2(case),
        other => Err(format!("unknown v2 case kind {other:?}")),
    }
}

fn run_file_case2(case: &Value) -> Result<(), String> {
    let rel = case["path"].as_str().expect("file case has path");
    let expect_kind = case["expect"].as_str().expect("case has expect");
    let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let dest = install_fixture2(vault.path(), rel);
    let source = fs::read(&dest).map_err(|e| format!("read vendored copy: {e}"))?;

    // `.base` fixtures are query definitions, not documents: the Reader
    // contract is safe-YAML validation + preservation, never execution.
    if rel.ends_with(".base") {
        let outcome = validate_base_query(&source);
        match expect_kind {
            "valid" | "valid_unexecuted" => {
                outcome.map_err(|d| format!("{rel}: {d}"))?;
            }
            "invalid_query" => {
                expect(outcome.is_err(), &format!("{rel} must be an invalid query"))?;
            }
            other => return Err(format!("unexpected expect {other:?} for .base fixture")),
        }
        return assert_bytes_unchanged(&dest, &source);
    }

    match expect_kind {
        "legacy_html" => {
            expect(
                classify_html_transport(&source) == HtmlClassification::Legacy,
                &format!("{rel} must classify as legacy HTML"),
            )?;
        }
        "legacy_markdown" => {
            expect(
                sniff_markdown(&source) == MarkdownClassification::Legacy,
                &format!("{rel} must classify as plain Markdown"),
            )?;
        }
        "invalid_query" => {
            expect(
                validate_base_query(&source).is_err(),
                &format!("{rel} must be an invalid query definition"),
            )?;
        }
        "valid_unexecuted" => {
            // Unknown constructs are preserved and never executed
            // (pdc-query/1 §4.6): a successful safe-YAML validation is the
            // whole Reader contract here.
            validate_base_query(&source).map_err(|d| format!("{rel}: {d}"))?;
        }
        _ => {
            let parsed = parse_document2(&dest, &source);
            match expect_kind {
                "valid" | "legacy_valid" => {
                    let doc = parsed.map_err(|d| format!("expected a valid document, got {d}"))?;
                    assert_v2_valid_document(
                        case["id"].as_str().unwrap_or_default(),
                        &doc,
                        expect_kind == "legacy_valid",
                    )?;
                }
                "unsafe_content" => {
                    let doc =
                        parsed.map_err(|d| format!("unsafe content still parses, got {d}"))?;
                    expect(
                        !doc.body.unsafe_constructs.is_empty(),
                        "expected non-empty body.unsafe_constructs",
                    )?;
                }
                _expected_code => {
                    let diag = parsed.expect_err("expected a diagnostic");
                    expect_code(&diag, case)?;
                }
            }
        }
    }

    assert_bytes_unchanged(&dest, &source)
}

/// Content assertions for the v2 valid fixtures. A new corpus pin that adds
/// another `valid` fixture must extend this match — parsing without content
/// assertions is not a conformance test.
fn assert_v2_valid_document(case_id: &str, doc: &PdcDocument, legacy: bool) -> Result<(), String> {
    expect(
        !legacy || doc.metadata.contract_version == 1,
        "legacy_valid fixtures must carry contract_version 1",
    )?;
    match case_id {
        "v2-md-minimal" => {
            expect(doc.metadata.contract_version == 2, "v2 envelope version")?;
            expect(doc.metadata.title == "Minimal v2 document", "title")?;
            expect(doc.display_title == "Minimal v2 document", "display title")?;
            expect(
                doc.body.text.contains("canonical Markdown under PDC 2."),
                "body text indexed",
            )?;
            expect(!doc.body.text.contains("format:"), "envelope never leaks")?;
        }
        "v2-md-semantics" => {
            let m = &doc.metadata;
            expect(m.contract_version == 2, "v2 envelope version")?;
            expect(m.tags == ["standard", "interop"], "tags")?;
            expect(m.aliases == ["Fixture"], "aliases")?;
            expect(m.cssclasses == ["wide-table"], "cssclasses")?;
            expect(m.favorite, "favorite")?;
            expect(m.profile.as_deref() == Some("note"), "profile")?;
            expect(m.lang.as_deref() == Some("en"), "lang")?;
            // Caret block IDs (heading + task line).
            expect(
                doc.body
                    .block_ids
                    .contains(&"b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32".into()),
                "heading block target",
            )?;
            expect(
                doc.body
                    .block_ids
                    .contains(&"b-018f47c6-c718-728c-9d91-b2bc700814bb".into()),
                "task block target",
            )?;
            // One canonical pdc:// link to the v1 minimal fixture.
            expect(
                doc.body
                    .document_links
                    .iter()
                    .any(|l| l.uuid.ends_with("9db8-0e5f9bcb17db") && !l.embed),
                "pdc:// link",
            )?;
            // Wiki links/embeds and the relative image, projected as written.
            expect(
                doc.body
                    .wiki_links
                    .iter()
                    .any(|w| w.target == "minimal" && w.embed),
                "wiki embed",
            )?;
            expect(
                doc.body
                    .wiki_links
                    .iter()
                    .any(|w| w.target == "assets/diagram.png" && w.embed),
                "relative image",
            )?;
            expect(doc.body.wiki_links.len() >= 3, "wiki + relative links")?;
            // GFM tasks: open+completed; `- [/]` is a preserved extension.
            expect(doc.body.tasks.len() == 2, "two standard tasks")?;
            expect(!doc.body.tasks[0].completed, "open task")?;
            // Benign raw HTML stays inert source; no unsafe flags.
            expect(
                doc.body.unsafe_constructs.is_empty(),
                "benign html not flagged",
            )?;
            expect(doc.body.text.contains("<b>bold</b>"), "raw html in source")?;
            expect(
                doc.body.text.contains("==Highlighted text=="),
                "highlight kept",
            )?;
            expect(doc.body.text.contains("%%source comment%%"), "comment kept")?;
            // The fenced `base` query validated and stays visible content.
            expect(doc.body.query_blocks == 1, "one valid base fence")?;
            expect(doc.body.query_errors.is_empty(), "no query errors")?;
            expect(
                doc.body.text.contains("```base"),
                "base fence stays in text",
            )?;
        }
        "v2-html-minimal" => {
            expect(doc.metadata.contract_version == 2, "v2 envelope version")?;
            expect(doc.metadata.title == "Minimal HTML v2", "title")?;
            expect(
                doc.metadata.transport_media_type()
                    == "application/vnd.pdc.document+html;version=2",
                "v2 html media type",
            )?;
        }
        "v2-html-semantics" => {
            expect(doc.metadata.contract_version == 2, "v2 envelope version")?;
            expect(doc.metadata.tags == ["portable", "html"], "tags")?;
            expect(
                doc.body
                    .block_ids
                    .contains(&"b-018f47c6-7dbe-7a14-9f67-6f89a5e3c170".into()),
                "h1 id target",
            )?;
            expect(
                doc.body
                    .asset_refs
                    .iter()
                    .any(|d| d.starts_with("0123456789abcdef")),
                "managed asset ref",
            )?;
            expect(doc.body.tasks.len() == 1, "one open task")?;
        }
        _ => {}
    }
    Ok(())
}

fn run_operation_case2(case: &Value) -> Result<(), String> {
    let op = case["operation"].as_str().unwrap_or_default();
    let input = case["input"].as_str().expect("operation has input");
    let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let dest = install_fixture2(vault.path(), input);
    let source = fs::read(&dest).map_err(|e| format!("read vendored copy: {e}"))?;

    match op {
        // Reader equivalents of the Writer operations: oxibrain never writes,
        // so the no-op/patch/conflict cases assert parse + byte preservation.
        "no-op-round-trip" | "metadata-patch" | "external-change-before-save" => {
            let parsed = parse_document2(&dest, &source);
            parsed.map_err(|d| format!("{op}: expected a readable document, got {d}"))?;
            assert_bytes_unchanged(&dest, &source)
        }
        "prefix-source-bytes" => {
            let prefix = decode_hex(case["prefixHex"].as_str().expect("prefixHex"));
            let mut bytes = prefix;
            bytes.extend_from_slice(&source);
            // Synthesize under a separate directory so the pristine vendored
            // copy stays byte-identical for the preservation assert.
            synthesize2(vault.path(), &format!("synth/{input}"), &bytes);
            assert_v2_diagnostic(vault.path(), &format!("synth/{input}"), case)?;
            assert_bytes_unchanged(&dest, &source)
        }
        "pad-body-to-total-bytes" => {
            let total = case["totalBytes"].as_u64().expect("totalBytes") as usize;
            let mut bytes = source.clone();
            bytes.resize(total, b'\n');
            synthesize2(vault.path(), &format!("synth/{input}"), &bytes);
            assert_v2_diagnostic(vault.path(), &format!("synth/{input}"), case)?;
            assert_bytes_unchanged(&dest, &source)
        }
        "replace-body-with-nested-block-quotes" => {
            // v1 complexity cap: nested block quotes replace the body.
            let depth = case["containerDepth"].as_u64().expect("containerDepth") as usize;
            let input_rel = format!("synth/{input}");
            let text = djot_with_body(
                &String::from_utf8(source.clone()).expect("djot fixture is UTF-8"),
                &nested_quotes_body(depth),
            );
            synthesize2(vault.path(), &input_rel, text.as_bytes());
            assert_v2_diagnostic(vault.path(), &input_rel, case)?;
            assert_bytes_unchanged(&dest, &source)
        }
        "replace-envelope-with-yaml-mapping-depth" => {
            let depth = case["depth"].as_u64().expect("depth") as usize;
            let mut envelope = String::from(
                "format: pdc-document/2\nbody: pdc-markdown/1\n\
                 id: 018f47c6-4a77-7c52-9db8-0e5f9bcb17db\n\
                 created: 2026-09-14T12:34:56.789Z\n\
                 updated: 2026-09-14T12:34:56.789Z\ntitle: Deep\n",
            );
            for i in 0..depth {
                envelope.push_str(&" ".repeat(i));
                envelope.push_str(&format!("k{i}:\n"));
            }
            envelope.push_str(&" ".repeat(depth));
            envelope.push_str("leaf: 1\n");
            let md = format!("---\n{envelope}---\n# Deep body\n");
            synthesize2(vault.path(), &format!("synth/{input}"), md.as_bytes());
            assert_v2_diagnostic(vault.path(), &format!("synth/{input}"), case)?;
            assert_bytes_unchanged(&dest, &source)
        }
        other => Err(format!("unknown v2 operation {other:?}")),
    }
}

/// Parse the synthesized copy and require the case's expected diagnostic.
fn assert_v2_diagnostic(vault: &Path, rel: &str, case: &Value) -> Result<(), String> {
    let bytes = fs::read(vault.join(rel)).map_err(|e| format!("read synthesized: {e}"))?;
    let parsed = parse_document2(Path::new(rel), &bytes);
    match case["expect"].as_str().unwrap_or_default() {
        "unsafe_content" => {
            let doc = parsed.map_err(|d| format!("unsafe content still parses, got {d}"))?;
            expect(
                !doc.body.unsafe_constructs.is_empty(),
                "expected unsafe_content",
            )?;
            Ok(())
        }
        code => {
            let diag = parsed.expect_err("expected a diagnostic");
            expect(
                diag.code.as_str() == code,
                &format!("expected diagnostic {code:?}, got {diag}"),
            )
        }
    }
}

fn synthesize2(vault: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let dest = vault.join(rel);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, bytes).unwrap();
    dest
}

/// Duplicate document IDs are a vault-level conflict: every member parses;
/// choosing the survivor is the facade's job and is asserted there.
fn run_set_case2(case: &Value) -> Result<(), String> {
    let paths = case["paths"].as_array().expect("set paths");
    let vault = tempdir().map_err(|e| format!("tempdir: {e}"))?;
    for rel in paths {
        let rel = rel.as_str().expect("path string");
        let dest = install_fixture2(vault.path(), rel);
        let source = fs::read(&dest).map_err(|e| format!("read vendored copy: {e}"))?;
        parse_document2(&dest, &source).map_err(|d| format!("{rel}: {d}"))?;
        assert_bytes_unchanged(&dest, &source)?;
    }
    Ok(())
}

/// Walk every vendored v2 fixture, parse it (any outcome), and assert the
/// source bytes on disk are identical afterwards.
#[test]
fn parsing_never_mutates_any_vendored_v2_fixture() {
    let root = Path::new(CORPUS2_ROOT).join("fixtures");
    let mut checked = 0usize;
    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let before = fs::read(path).unwrap();
        match path.extension().and_then(|e| e.to_str()) {
            Some("base") => {
                let _ = validate_base_query(&before);
            }
            Some("md") | Some("html") | Some("djot") => {
                let _ = parse_document2(path, &before);
            }
            _ => continue,
        }
        let after = fs::read(path).unwrap();
        assert_eq!(before, after, "source mutated: {}", path.display());
        checked += 1;
    }
    assert!(
        checked > 40,
        "expected the full vendored fixture set, got {checked}"
    );
}
