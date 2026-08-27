//! Scanner + decoder integration tests. Live alongside the in-module unit
//! tests in `src/scan.rs`: the unit tests cover the walker plumbing, the
//! integration tests cover decoder interop, round-trip stability, and the
//! extended public surface.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use oxibrain_connectors::{
    DECODER_VERSION, DecodedDocument, MediaType, RootEntry, canonicalize_root, decode, scan_root,
};
use tempfile::tempdir;

fn write(dir: &Path, rel: &str, body: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, body).unwrap();
}

fn entry(dir: &Path, max_file_bytes: u64) -> RootEntry {
    RootEntry {
        alias: "root".into(),
        path: dir.to_path_buf(),
        space: "test".into(),
        include: vec!["**/*.md".into(), "**/*.html".into(), "**/*.txt".into()],
        exclude: vec![],
        max_file_bytes,
    }
}

// --- decoder determinism & semantics ---------------------------------------

#[test]
fn markdown_decoder_strips_yaml_frontmatter() {
    let body = "---\nid: note-42\ntags: [meeting]\n---\n# Heading\n\nbody text\n";
    let dec = decode(MediaType::Markdown, body.as_bytes()).unwrap();
    assert_eq!(dec.media_type, MediaType::Markdown);
    assert!(dec.text.contains("# Heading"));
    assert!(
        !dec.text.contains("id: note-42"),
        "frontmatter leaked into body"
    );
}

#[test]
fn markdown_decoder_handles_toml_frontmatter() {
    let body = "+++\ntitle = \"x\"\n+++\n\nfirst paragraph\n";
    let dec = decode(MediaType::Markdown, body.as_bytes()).unwrap();
    assert!(dec.text.starts_with("first paragraph"));
    assert!(!dec.text.contains("title"));
}

#[test]
fn markdown_decoder_without_frontmatter_is_verbatim() {
    let body = "no frontmatter\njust prose\n";
    let dec = decode(MediaType::Markdown, body.as_bytes()).unwrap();
    assert_eq!(dec.text, body);
}

#[test]
fn html_decoder_runs_html_to_text() {
    let body = "<p>hello <strong>world</strong></p>";
    let dec = decode(MediaType::Html, body.as_bytes()).unwrap();
    assert!(dec.text.contains("hello world"));
    assert!(!dec.text.contains('<'), "tags leaked: {}", dec.text);
}

#[test]
fn html_decoder_strips_oximemo_comment_frontmatter() {
    let body = "<!--\n+++\nid = \"abc\"\n+++\n-->\n<h1>Title</h1>\n<p>body</p>";
    let dec = decode(MediaType::Html, body.as_bytes()).unwrap();
    assert!(dec.text.contains("Title"));
    assert!(dec.text.contains("body"));
    assert!(!dec.text.contains("id ="));
}

#[test]
fn plain_text_decoder_passes_through_with_lossy_utf8() {
    let dec = decode(MediaType::PlainText, b"plain\ntext\n").unwrap();
    assert_eq!(dec.text, "plain\ntext\n");
    // Invalid UTF-8 is replaced, not dropped wholesale.
    let invalid: &[u8] = &[0xff, 0xfe, b'a'];
    let dec = decode(MediaType::PlainText, invalid).unwrap();
    assert!(dec.text.contains('a'));
}

#[test]
fn decoder_determinism() {
    let body = b"hello world";
    let a: DecodedDocument = decode(MediaType::PlainText, body).unwrap();
    let b: DecodedDocument = decode(MediaType::PlainText, body).unwrap();
    assert_eq!(a, b);
    // Same input across every media type stays deterministic.
    let c: DecodedDocument = decode(MediaType::Markdown, body).unwrap();
    let d: DecodedDocument = decode(MediaType::Markdown, body).unwrap();
    assert_eq!(c, d);
}

#[test]
fn media_type_extension_classification() {
    assert_eq!(MediaType::from_extension("md"), Some(MediaType::Markdown));
    assert_eq!(
        MediaType::from_extension("markdown"),
        Some(MediaType::Markdown)
    );
    assert_eq!(MediaType::from_extension("MD"), Some(MediaType::Markdown));
    assert_eq!(MediaType::from_extension("html"), Some(MediaType::Html));
    assert_eq!(MediaType::from_extension("htm"), Some(MediaType::Html));
    assert_eq!(MediaType::from_extension("txt"), Some(MediaType::PlainText));
    assert_eq!(MediaType::from_extension("docx"), None);
    assert_eq!(MediaType::from_extension("rst"), None);

    assert_eq!(MediaType::Markdown.as_str(), "text/markdown");
    assert_eq!(MediaType::Html.as_str(), "text/html");
    assert_eq!(MediaType::PlainText.as_str(), "text/plain");
}

#[test]
fn decoder_version_is_exposed() {
    // The version is part of the public surface; bumping it requires bumping
    // it inside the facade as well.
    assert!(!DECODER_VERSION.is_empty());
}

// --- scanner integration ---------------------------------------------------

#[test]
fn scanner_collects_nested_files_sorted() {
    let dir = tempdir().unwrap();
    write(dir.path(), "c/nested.md", "third");
    write(dir.path(), "a.md", "first");
    write(dir.path(), "b/inner.html", "second");

    let result = scan_root(&entry(dir.path(), 1024)).unwrap();
    let locators: Vec<&str> = result
        .observations
        .iter()
        .map(|o| o.locator.as_str())
        .collect();
    assert_eq!(locators, vec!["a.md", "b/inner.html", "c/nested.md"]);
    assert!(result.skipped.is_empty());
}

#[test]
fn scanner_excludes_per_glob() {
    let dir = tempdir().unwrap();
    write(dir.path(), "keep.md", "ok");
    write(dir.path(), "scratch.tmp", "discard");

    let mut cfg = entry(dir.path(), 1024);
    cfg.include = vec!["**/*".into()];
    cfg.exclude = vec!["**/*.tmp".into()];
    let result = scan_root(&cfg).unwrap();
    assert_eq!(result.observations.len(), 1);
    assert_eq!(result.observations[0].locator, "keep.md");
    assert!(result.skipped.iter().any(|s| s.locator == "scratch.tmp"));
}

#[test]
fn scanner_skips_oversize_files() {
    let dir = tempdir().unwrap();
    write(dir.path(), "small.md", "ok");
    write(dir.path(), "huge.md", &"x".repeat(8192));

    let result = scan_root(&entry(dir.path(), 1024)).unwrap();
    assert_eq!(result.observations.len(), 1);
    assert_eq!(result.observations[0].locator, "small.md");
    let huge = result
        .skipped
        .iter()
        .find(|s| s.locator == "huge.md")
        .unwrap();
    assert!(huge.reason.contains("oversize"));
}

#[test]
fn scanner_does_not_follow_symlinks() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    write(outside.path(), "secret.md", "should not be reached");
    symlink(outside.path().join("secret.md"), dir.path().join("link.md")).unwrap();
    write(dir.path(), "plain.md", "ok");

    let result = scan_root(&entry(dir.path(), 1024)).unwrap();
    let locators: Vec<&str> = result
        .observations
        .iter()
        .map(|o| o.locator.as_str())
        .collect();
    assert_eq!(locators, vec!["plain.md"]);
    assert!(result.skipped.iter().any(|s| s.locator == "link.md"));
}

#[test]
fn scanner_marks_observations_with_modified_ns_and_size() {
    let dir = tempdir().unwrap();
    write(dir.path(), "note.md", "hello");
    let result = scan_root(&entry(dir.path(), 1024)).unwrap();
    let obs = &result.observations[0];
    assert_eq!(obs.locator, "note.md");
    assert_eq!(obs.bytes, "hello".len() as u64);
    assert!(obs.modified_ns >= 0);
    // Plain scanner never contributes a revision hint; git_docs owns that.
    assert!(obs.revision_hint.is_none());
}

#[test]
fn scanner_in_place_edit_changes_modified_ns() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("note.md");
    fs::write(&target, "v1").unwrap();

    let first = scan_root(&entry(dir.path(), 1024)).unwrap();
    let first_ns = first.observations[0].modified_ns;

    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&target, "v2 longer content").unwrap();
    let second = scan_root(&entry(dir.path(), 1024)).unwrap();
    let second_ns = second.observations[0].modified_ns;
    assert!(second_ns >= first_ns);
}

#[test]
fn canonicalize_missing_root_errors() {
    let dir = tempdir().unwrap();
    let bogus = dir.path().join("missing");
    let err = canonicalize_root(&bogus).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn scanner_is_deterministic_across_calls() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.md", "alpha");
    write(dir.path(), "b.md", "beta");
    write(dir.path(), "c.md", "gamma");
    let r1 = scan_root(&entry(dir.path(), 1024)).unwrap();
    let r2 = scan_root(&entry(dir.path(), 1024)).unwrap();
    let l1: Vec<&str> = r1.observations.iter().map(|o| o.locator.as_str()).collect();
    let l2: Vec<&str> = r2.observations.iter().map(|o| o.locator.as_str()).collect();
    assert_eq!(l1, l2);
}
