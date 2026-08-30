//! `documents.toml` config: round-trip, missing file, ~ expansion,
//! duplicate-alias validation.

use std::fs;

use oxibrain_connectors::{ConfigError, DocumentsConfig, RootEntry, UpsertOutcome};
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

fn entry(alias: &str, space: &str, path: &str, max_file_bytes: u64) -> RootEntry {
    RootEntry {
        alias: alias.into(),
        path: path.into(),
        space: space.into(),
        include: vec!["**/*.md".into()],
        exclude: vec!["**/.git/**".into()],
        max_file_bytes,
    }
}

#[test]
fn upsert_added_replaced_unchanged() {
    let mut cfg = DocumentsConfig::default();
    let first = entry("vault", "personal", "/tmp/vault", 42);
    assert_eq!(cfg.upsert(first.clone()), UpsertOutcome::Added);
    // Identical re-registration is a no-op (idempotence).
    assert_eq!(cfg.upsert(first.clone()), UpsertOutcome::Unchanged);
    assert_eq!(cfg.roots.len(), 1);
    // Different rules under the same alias replace in place — no duplicate.
    let changed = entry("vault", "personal", "/tmp/vault", 1024);
    assert_eq!(cfg.upsert(changed), UpsertOutcome::Replaced);
    assert_eq!(cfg.roots.len(), 1);
    assert_eq!(cfg.roots[0].max_file_bytes, 1024);
    // A second alias appends.
    assert_eq!(
        cfg.upsert(entry("docs", "work", "/tmp/docs", 42)),
        UpsertOutcome::Added
    );
    assert_eq!(cfg.roots.len(), 2);
}

#[test]
fn save_round_trips_upsert_and_is_atomic() {
    let dir = tempdir().unwrap();
    let mut cfg = DocumentsConfig::default();
    assert_eq!(
        cfg.upsert(entry("vault", "personal", "/tmp/vault", 42)),
        UpsertOutcome::Added
    );
    DocumentsConfig::save(dir.path(), &cfg).unwrap();
    // No temp file residue after a completed save.
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let reloaded = DocumentsConfig::load(dir.path()).unwrap();
    assert_eq!(reloaded, cfg);
}

#[test]
fn upsert_collapses_duplicate_alias_pollution() {
    // Flat-era pollution: several [[root]] blocks share the alias
    // "vault". Registration must repair them down to one entry instead
    // of failing validation forever.
    fn root(alias: &str, path: &str) -> RootEntry {
        RootEntry {
            alias: alias.into(),
            path: path.into(),
            space: "personal".into(),
            include: oxibrain_connectors::documents_config::default_include(),
            exclude: oxibrain_connectors::documents_config::default_exclude(),
            max_file_bytes: oxibrain_connectors::documents_config::DEFAULT_MAX_FILE_BYTES,
        }
    }
    let mut cfg = DocumentsConfig {
        roots: vec![
            root("vault", "/old/wrong"),
            root("other", "/tmp/other"),
            root("vault", "/tmp/gone"),
        ],
    };
    let incoming = root("vault", "/new/vault");
    assert_eq!(cfg.upsert(incoming.clone()), UpsertOutcome::Replaced);
    cfg.validate().expect("duplicates collapsed");
    assert_eq!(cfg.roots.len(), 2, "other alias survives: {:?}", cfg.roots);
    let vaults: Vec<&RootEntry> = cfg.roots.iter().filter(|r| r.alias == "vault").collect();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].path, incoming.path);
    // The collapsed config still round-trips through save/load.
    let dir = tempdir().unwrap();
    DocumentsConfig::save(dir.path(), &cfg).unwrap();
    assert_eq!(DocumentsConfig::load(dir.path()).unwrap(), cfg);
}
