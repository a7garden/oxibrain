//! `documents.toml` config: round-trip, missing file, ~ expansion,
//! duplicate-alias validation.

use std::fs;

use oxibrain_connectors::{ConfigError, DocumentsConfig, RootEntry};
use tempfile::tempdir;

const SAMPLE: &str = r#"
[[root]]
alias = "vault"
path = "~/.oxi/vault"
space = "personal"
include = ["**/*.md", "**/*.txt"]
exclude = ["**/.git/**", "**/.DS_Store"]
max_file_bytes = 4096

[[root]]
alias = "handbook"
path = "~/work/handbook"
space = "work"
"#;

#[test]
fn round_trip_preserves_fields() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("documents.toml"), SAMPLE).unwrap();

    let cfg = DocumentsConfig::load(dir.path()).expect("load");
    cfg.validate().expect("validate");
    assert_eq!(cfg.roots.len(), 2);

    let vault = cfg.root("vault").unwrap();
    assert_eq!(vault.alias, "vault");
    assert_eq!(vault.space, "personal");
    assert_eq!(vault.include, vec!["**/*.md", "**/*.txt"]);
    assert_eq!(vault.exclude, vec!["**/.git/**", "**/.DS_Store"]);
    assert_eq!(vault.max_file_bytes, 4096);

    let handbook = cfg.root("handbook").unwrap();
    assert_eq!(
        handbook.max_file_bytes,
        oxibrain_connectors::documents_config::DEFAULT_MAX_FILE_BYTES
    );

    // Save and reload — must be byte-identical in parsed form.
    DocumentsConfig::save(dir.path(), &cfg).unwrap();
    let reloaded = DocumentsConfig::load(dir.path()).unwrap();
    assert_eq!(reloaded, cfg);
}

#[test]
fn missing_file_yields_empty_config() {
    let dir = tempdir().unwrap();
    let cfg = DocumentsConfig::load(dir.path()).unwrap();
    assert!(cfg.roots.is_empty());
}

#[test]
fn tilde_expansion_against_home() {
    let dir = tempdir().unwrap();
    let body = r#"
[[root]]
alias = "home-root"
path = "~/.some-vault"
space = "personal"
"#;
    fs::write(dir.path().join("documents.toml"), body).unwrap();

    // Force a known HOME so the test is hermetic across environments.
    let scratch = tempdir().unwrap();
    let prev_home = std::env::var_os("HOME");
    // SAFETY: single-threaded test; the local var is restored before return.
    unsafe { std::env::set_var("HOME", scratch.path()) };

    let cfg = DocumentsConfig::load(dir.path()).unwrap();
    match prev_home {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let root = cfg.root("home-root").unwrap();
    let expected = scratch.path().join(".some-vault");
    assert_eq!(root.path, expected);
}

#[test]
fn duplicate_alias_rejected() {
    let body = r#"
[[root]]
alias = "dup"
path = "/tmp/a"
space = "personal"

[[root]]
alias = "dup"
path = "/tmp/b"
space = "personal"
"#;
    let cfg: DocumentsConfig = toml::from_str(body).unwrap();
    let err = cfg.validate().unwrap_err();
    matches!(err, ConfigError::Invalid(_));
}

#[test]
fn empty_alias_rejected() {
    let body = r#"
[[root]]
alias = ""
path = "/tmp/a"
space = "personal"
"#;
    let cfg: DocumentsConfig = toml::from_str(body).unwrap();
    assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
}

#[test]
fn empty_space_rejected() {
    let body = r#"
[[root]]
alias = "vault"
path = "/tmp/a"
space = ""
"#;
    let cfg: DocumentsConfig = toml::from_str(body).unwrap();
    assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
}

#[test]
fn zero_max_bytes_rejected() {
    let body = r#"
[[root]]
alias = "vault"
path = "/tmp/a"
space = "personal"
max_file_bytes = 0
"#;
    let cfg: DocumentsConfig = toml::from_str(body).unwrap();
    assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
}

#[test]
fn malformed_toml_returns_parse_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("documents.toml"), "not = \"valid = toml = ").unwrap();
    let err = DocumentsConfig::load(dir.path()).unwrap_err();
    matches!(err, ConfigError::Parse { .. });
}

#[test]
fn root_lookup_helpers() {
    let entries = vec![RootEntry {
        alias: "alias-a".into(),
        path: "/tmp/a".into(),
        space: "personal".into(),
        include: vec![],
        exclude: vec![],
        max_file_bytes: 1024,
    }];
    let cfg = DocumentsConfig { roots: entries };
    assert!(cfg.root("alias-a").is_some());
    assert!(cfg.root("missing").is_none());
}
