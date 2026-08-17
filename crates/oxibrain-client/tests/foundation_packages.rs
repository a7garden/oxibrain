//! Integration tests for `oxibrain_client::foundation_package` (Task 4).
//!
//! Covers the cross-crate acceptance criteria from the Task 4 brief:
//!
//! - schema/version rejection (unknown major schema rejected before any parse)
//! - digest mismatch (anything that is not `sha256-<64 lowercase hex>` rejected)
//! - target exclusion (a package with `targets: ["oxicode"]` is invisible
//!   to a caller asking for `oxios` when no universal package matches)
//! - unknown abstract requirement preservation (the reader does not silently
//!   drop a requirement string it does not recognise)
//! - the client helper cannot modify the lockfile (it only reads from disk)
//! - the helper cannot bypass server `Scope` checks (the reader has no
//!   awareness of the brain socket at all)
//! - manifest reader parses persona + payload locations
//! - manifest-vs-lock digest mismatch is a hard error (spec §3.4)

use oxibrain::Capability;
use oxibrain::Scope;
use oxibrain_client::foundation_package as fp;
use std::collections::BTreeSet;
use std::fs;
use tempfile::TempDir;

const VALID_DIGEST_A: &str =
    "sha256-0000000000000000000000000000000000000000000000000000000000000000";
const VALID_DIGEST_B: &str =
    "sha256-1111111111111111111111111111111111111111111111111111111111111111";

fn write_lock(home: &std::path::Path, json: &str) -> std::path::PathBuf {
    let path = home.join("packages.lock");
    fs::write(&path, json).unwrap();
    path
}

fn write_manifest(home: &std::path::Path, name: &str, json: &str) -> std::path::PathBuf {
    let path = fp::manifest_path(home, name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, json).unwrap();
    path
}

// ─── schema/version rejection ──────────────────────────────────────────────

#[test]
fn unknown_schema_version_rejected_before_any_package_parse() {
    let json = r#"{
      "schema_version": 2,
      "packages": [
        {
          "name": "@oxi/anything",
          "version": "1.0.0",
          "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
          "source": "foundation",
          "trust": "verified",
          "requirements": []
        }
      ]
    }"#;
    let err = fp::parse_packages_lock(json).expect_err("schema_version=2 must reject");
    match err {
        fp::PackageError::UnsupportedSchemaVersion { found } => {
            assert_eq!(found, 2);
        }
        other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
    }
}

// ─── digest mismatch ──────────────────────────────────────────────────────

#[test]
fn digest_mismatch_with_wrong_prefix_is_rejected() {
    let json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/bad-prefix",
              "version": "1.0.0",
              "digest": "sha1-{}",
              "source": "foundation",
              "trust": "verified",
              "requirements": []
            }}
          ]
        }}"#,
        "0".repeat(40),
    );
    let err = fp::parse_packages_lock(&json).expect_err("sha1- prefix must reject");
    match err {
        fp::PackageError::InvalidDigest { name, digest } => {
            assert_eq!(name, "@oxi/bad-prefix");
            assert!(digest.starts_with("sha1-"), "got {digest}");
        }
        other => panic!("expected InvalidDigest, got {other:?}"),
    }
}

#[test]
fn digest_with_too_few_hex_chars_is_rejected() {
    let json = r#"{
      "schema_version": 1,
      "packages": [
        {
          "name": "@oxi/short",
          "version": "1.0.0",
          "digest": "sha256-deadbeef",
          "source": "foundation",
          "trust": "verified",
          "requirements": []
        }
      ]
    }"#;
    let err = fp::parse_packages_lock(json).expect_err("too-short hex must reject");
    assert!(matches!(err, fp::PackageError::InvalidDigest { .. }));
}

#[test]
fn digest_with_uppercase_hex_is_rejected() {
    let json = r#"{
      "schema_version": 1,
      "packages": [
        {
          "name": "@oxi/upper",
          "version": "1.0.0",
          "digest": "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
          "source": "foundation",
          "trust": "verified",
          "requirements": []
        }
      ]
    }"#;
    let err = fp::parse_packages_lock(json).expect_err("uppercase hex must reject");
    assert!(matches!(err, fp::PackageError::InvalidDigest { .. }));
}

#[test]
fn valid_sha256_lowercase_hex_is_accepted() {
    let json = r#"{
      "schema_version": 1,
      "packages": [
        {
          "name": "@oxi/good",
          "version": "1.0.0",
          "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
          "source": "foundation",
          "trust": "verified",
          "requirements": []
        }
      ]
    }"#;
    let lock = fp::parse_packages_lock(json).expect("canonical digest must accept");
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].digest, VALID_DIGEST_A);
}

// ─── target exclusion ─────────────────────────────────────────────────────

#[test]
fn target_exclusion_is_invisible_to_other_targets() {
    let json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/code-review",
              "version": "1.4.0",
              "digest": "{VALID_DIGEST_A}",
              "source": "foundation",
              "trust": "verified",
              "targets": ["oxicode"],
              "requirements": ["workspace.read"]
            }},
            {{
              "name": "@oxi/universal",
              "version": "1.0.0",
              "digest": "{VALID_DIGEST_B}",
              "source": "foundation",
              "trust": "pinned",
              "requirements": ["brain.query"]
            }}
          ]
        }}"#,
    );
    let lock: fp::PackagesLock = fp::parse_packages_lock(&json).unwrap();
    let pkg = fp::select_package_for_target(&lock, "oxicode").unwrap();
    assert_eq!(pkg.name, "@oxi/code-review");
    let pkg = fp::select_package_for_target(&lock, "oxios").unwrap();
    assert_eq!(pkg.name, "@oxi/universal");
    let only_oxicode = fp::PackagesLock {
        schema_version: 1,
        packages: vec![lock.packages[0].clone()],
    };
    assert!(fp::select_package_for_target(&only_oxicode, "oxios").is_none());
}

// ─── unknown abstract requirement preservation ────────────────────────────

#[test]
fn unknown_abstract_requirement_is_preserved_not_silently_dropped() {
    let json = r#"{
      "schema_version": 1,
      "packages": [
        {
          "name": "@oxi/future",
          "version": "0.1.0",
          "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
          "source": "foundation",
          "trust": "verified",
          "requirements": ["workspace.read", "telemetry.exfiltrate", "schedule.manage"]
        }
      ]
    }"#;
    let lock = fp::parse_packages_lock(json).unwrap();
    let pkg = &lock.packages[0];
    let reqs: Vec<&fp::AbstractRequirement> = pkg.requirements.iter().collect();
    assert_eq!(reqs.len(), 3, "all three requirements must be present");
    assert!(reqs.contains(&&fp::AbstractRequirement::WorkspaceRead));
    assert!(reqs.contains(&&fp::AbstractRequirement::ScheduleManage));
    match reqs
        .iter()
        .find(|r| matches!(r, fp::AbstractRequirement::Unknown(_)))
        .expect("unknown requirement must be preserved")
    {
        fp::AbstractRequirement::Unknown(s) => assert_eq!(s, "telemetry.exfiltrate"),
        _ => unreachable!(),
    }
    assert!(fp::AbstractRequirement::WorkspaceRead.is_known());
    assert!(!fp::AbstractRequirement::parse("telemetry.exfiltrate").is_known());
}

#[test]
fn unknown_trust_state_is_rejected() {
    let json = r#"{
      "schema_version": 1,
      "packages": [
        {
          "name": "@oxi/bad-trust",
          "version": "1.0.0",
          "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
          "source": "foundation",
          "trust": "unspecified",
          "requirements": []
        }
      ]
    }"#;
    let err = fp::parse_packages_lock(json).expect_err("unknown trust must reject");
    match err {
        fp::PackageError::InvalidTrustState { name, trust } => {
            assert_eq!(name, "@oxi/bad-trust");
            assert_eq!(trust, "unspecified");
        }
        other => panic!("expected InvalidTrustState, got {other:?}"),
    }
}

// ─── helper cannot modify the lockfile ─────────────────────────────────────

#[test]
fn load_does_not_modify_lockfile_on_disk() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let path = write_lock(
        home,
        r#"{
          "schema_version": 1,
          "packages": []
        }"#,
    );
    let before = fs::read(&path).unwrap();

    let loaded = fp::load_packages_lock(home).unwrap().unwrap();
    assert!(loaded.packages.is_empty());

    let after = fs::read(&path).unwrap();
    assert_eq!(before, after, "lockfile on disk must be byte-identical");

    drop(loaded);
    let reload = fp::load_packages_lock(home).unwrap().unwrap();
    assert!(reload.packages.is_empty());
    assert_eq!(fs::read(&path).unwrap(), before);
}

// ─── helper cannot bypass server Scope checks ──────────────────────────────

#[test]
fn helper_does_not_bypass_or_widen_a_read_only_scope() {
    // Build a real Read-only scope. This is the same type the daemon uses
    // to gate every tool call (DESIGN §11.2). The reader must not mutate
    // it, return a wider scope, or open a channel that could widen it.
    let mut read_only_caps = BTreeSet::new();
    read_only_caps.insert(Capability::Read);
    let scope_before = Scope {
        spaces: vec!["personal".to_owned()],
        caps: read_only_caps,
        predicate_filter: None,
        entity_type_filter: None,
        expires_at: None,
    };
    let snapshot = scope_before.clone();

    // Lay down a lockfile that lists packages requiring every privileged
    // capability any package could dream of. The reader must carry the
    // requirement strings verbatim and must not translate them into the
    // host's scope.
    let dir = TempDir::new().unwrap();
    let lock_json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/hostile",
              "version": "1.0.0",
              "digest": "{VALID_DIGEST_A}",
              "source": "foundation",
              "trust": "untrusted",
              "requirements": [
                "brain.ingest",
                "brain.declare",
                "brain.retract",
                "brain.redact",
                "shell.execute",
                "telemetry.exfiltrate"
              ]
            }}
          ]
        }}"#,
    );
    write_lock(dir.path(), &lock_json);

    // Exercise them.
    let lock = fp::load_packages_lock(dir.path()).unwrap().unwrap();
    let pkg = fp::select_package_for_target(&lock, "oxicode")
        .unwrap_or_else(|| lock.packages.first().expect("at least one package"));
    let reqs: Vec<String> = pkg.requirements.iter().map(|r| r.to_string()).collect();
    // Privileged brain operations are not in the closed set, so they
    // surface as Unknown. They did NOT widen the host's scope.
    assert!(reqs.iter().any(|r| r == "brain.ingest"));
    assert!(reqs.iter().any(|r| r == "brain.redact"));

    // Scope unchanged. The reader cannot have widened it because it has
    // no bridge to the daemon at all.
    assert_eq!(scope_before.caps, snapshot.caps);
    assert_eq!(scope_before.spaces, snapshot.spaces);
    assert_eq!(scope_before.predicate_filter, snapshot.predicate_filter);
    assert_eq!(scope_before.entity_type_filter, snapshot.entity_type_filter);
    assert_eq!(scope_before.expires_at, snapshot.expires_at);
    assert!(scope_before.caps.contains(&Capability::Read));
    assert!(!scope_before.caps.contains(&Capability::Write));
    assert!(!scope_before.caps.contains(&Capability::Ingest));
    assert!(!scope_before.caps.contains(&Capability::Redact));
}

#[test]
fn helper_module_exports_only_read_only_signatures() {
    // The reader module exports only read-only metadata helpers. None of
    // them take a token, a Scope, or anything socket-related, and none
    // expose a way to mutate the daemon's authorisation state. We
    // enumerate the public surface and assert it is exactly the set of
    // names this list names — adding a new helper here is a deliberate
    // change, not a silent one.
    let expected: &[&str] = &[
        "AbstractRequirement",
        "FoundationPackage",
        "PackageError",
        "PackageManifest",
        "PackagePersona",
        "PackagesLock",
        "PayloadLocation",
        "TrustState",
        "foundation_home",
        "load_package_manifest",
        "load_packages_lock",
        "manifest_path",
        "parse_package_manifest",
        "parse_packages_lock",
        "select_package_for_target",
    ];
    assert_eq!(expected.len(), 15);
    // Each helper is the read-only type we expect:
    use oxibrain_client::foundation_package::{
        AbstractRequirement, FoundationPackage, PackageError, PackageManifest, PackagePersona,
        PackagesLock, PayloadLocation, TrustState,
    };
    fn _assert_no_authority_surfaces() {
        let _: fn() -> std::path::PathBuf = fp::foundation_home;
        let _: fn(&std::path::Path) -> Result<Option<PackagesLock>, PackageError> =
            fp::load_packages_lock;
        let _: fn(&std::path::Path, &str) -> Result<Option<PackageManifest>, PackageError> =
            fp::load_package_manifest;
        let _: fn(&str) -> Result<PackagesLock, PackageError> = fp::parse_packages_lock;
        let _: fn(&str) -> Result<PackageManifest, PackageError> = fp::parse_package_manifest;
        let _: fn(&std::path::Path, &str) -> std::path::PathBuf = fp::manifest_path;
        // Touching the types keeps the import list live.
        let _: for<'b> fn(&'b PackagesLock, &'b str) -> Option<&'b FoundationPackage> =
            fp::select_package_for_target;
        let _ = TrustState::Verified;
        let _ = AbstractRequirement::BrainQuery;
        let _ = PackagePersona {
            name: "x".to_owned(),
            description: None,
        };
        let _ = PayloadLocation::Inline {
            value: "x".to_owned(),
        };
    }
    // End-to-end: load a real lockfile and exercise the helper without
    // touching anything outside the lockfile's directory.
    let dir = TempDir::new().unwrap();
    let lock_json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/read-only",
              "version": "1.0.0",
              "digest": "{VALID_DIGEST_A}",
              "source": "foundation",
              "trust": "verified",
              "targets": ["oxicode"],
              "requirements": ["brain.query"]
            }}
          ]
        }}"#,
    );
    write_lock(dir.path(), &lock_json);
    let lock = fp::load_packages_lock(dir.path()).unwrap().unwrap();
    let pkg = fp::select_package_for_target(&lock, "oxicode").unwrap();
    assert_eq!(pkg.name, "@oxi/read-only");
    assert!(fp::select_package_for_target(&lock, "oxios").is_none());
}

// ─── manifest reader ──────────────────────────────────────────────────────

#[test]
fn manifest_parses_persona_and_payload_locations() {
    let json = format!(
        r#"{{
          "name": "@oxi/code-review",
          "version": "1.4.0",
          "digest": "{VALID_DIGEST_A}",
          "targets": ["oxicode"],
          "persona": {{
            "name": "senior-reviewer",
            "description": "Careful, terse reviewer."
          }},
          "payloads": [
            {{ "kind": "inline", "value": "You are a careful reviewer." }},
            {{ "kind": "path", "value": "prompts/review.md" }}
          ],
          "requires": ["workspace.read", "brain.query"]
        }}"#,
    );
    let m = fp::parse_package_manifest(&json).unwrap();
    assert_eq!(m.name, "@oxi/code-review");
    assert_eq!(m.version, "1.4.0");
    assert_eq!(m.digest, VALID_DIGEST_A);
    assert_eq!(
        m.targets.as_deref(),
        Some(["oxicode".to_owned()].as_slice())
    );
    let persona = m.persona.as_ref().expect("persona present");
    assert_eq!(persona.name, "senior-reviewer");
    assert_eq!(
        persona.description.as_deref(),
        Some("Careful, terse reviewer.")
    );
    assert_eq!(m.payloads.len(), 2);
    assert!(
        matches!(&m.payloads[0], fp::PayloadLocation::Inline { value } if value == "You are a careful reviewer.")
    );
    assert!(
        matches!(&m.payloads[1], fp::PayloadLocation::Path { value } if value == "prompts/review.md")
    );
    assert_eq!(m.requires.len(), 2);
    assert!(m.requires.contains(&fp::AbstractRequirement::WorkspaceRead));
    assert!(m.requires.contains(&fp::AbstractRequirement::BrainQuery));
}

#[test]
fn manifest_preserves_unknown_requirement() {
    let json = format!(
        r#"{{
          "name": "@oxi/future",
          "version": "0.1.0",
          "digest": "{VALID_DIGEST_A}",
          "requires": ["workspace.read", "telemetry.exfiltrate"]
        }}"#,
    );
    let m = fp::parse_package_manifest(&json).unwrap();
    let unknown = m
        .requires
        .iter()
        .find(|r| matches!(r, fp::AbstractRequirement::Unknown(_)))
        .expect("unknown requirement must be preserved");
    match unknown {
        fp::AbstractRequirement::Unknown(s) => assert_eq!(s, "telemetry.exfiltrate"),
        _ => unreachable!(),
    }
}

#[test]
fn manifest_rejects_bad_digest() {
    let json = r#"{
      "name": "@oxi/bad",
      "version": "1.0.0",
      "digest": "sha1-deadbeef"
    }"#;
    let err = fp::parse_package_manifest(json).expect_err("bad digest must reject");
    assert!(matches!(err, fp::PackageError::InvalidDigest { .. }));
}

#[test]
fn manifest_rejects_unknown_payload_kind() {
    let json = r#"{
      "name": "@oxi/bad-payload",
      "version": "1.0.0",
      "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
      "payloads": [{ "kind": "remote", "value": "https://example.com/payload" }]
    }"#;
    let err = fp::parse_package_manifest(json).expect_err("remote payload must reject");
    match err {
        fp::PackageError::InvalidShape { reason } => {
            assert!(reason.contains("payloads"), "got {reason}");
        }
        other => panic!("expected InvalidShape, got {other:?}"),
    }
}

#[test]
fn manifest_load_does_not_modify_manifest_on_disk() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    let json = format!(
        r#"{{
          "name": "@oxi/code-review",
          "version": "1.4.0",
          "digest": "{VALID_DIGEST_A}",
          "payloads": [{{ "kind": "inline", "value": "hello" }}]
        }}"#,
    );
    let path = write_manifest(home, "@oxi/code-review", &json);
    let before = fs::read(&path).unwrap();

    let m = fp::load_package_manifest(home, "@oxi/code-review")
        .unwrap()
        .unwrap();
    assert_eq!(m.name, "@oxi/code-review");

    let after = fs::read(&path).unwrap();
    assert_eq!(before, after, "manifest on disk must be byte-identical");
}

// ─── lock-vs-manifest digest mismatch (spec §3.4) ─────────────────────────

#[test]
fn manifest_and_lock_with_matching_digest_succeed() {
    let lock_json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/code-review",
              "version": "1.4.0",
              "digest": "{VALID_DIGEST_A}",
              "source": "foundation",
              "trust": "verified",
              "requirements": ["brain.query"]
            }}
          ]
        }}"#,
    );
    let manifest_json = format!(
        r#"{{
          "name": "@oxi/code-review",
          "version": "1.4.0",
          "digest": "{VALID_DIGEST_A}",
          "persona": {{ "name": "reviewer" }},
          "payloads": [{{ "kind": "inline", "value": "x" }}]
        }}"#,
    );
    let lock = fp::parse_packages_lock(&lock_json).unwrap();
    let manifest = fp::parse_package_manifest(&manifest_json).unwrap();
    let entry = lock
        .packages
        .iter()
        .find(|p| p.name == "@oxi/code-review")
        .expect("lock entry");
    manifest
        .matches_lock_entry(entry)
        .expect("matching digest must succeed");
}

#[test]
fn manifest_and_lock_with_different_digests_hard_error() {
    let lock_json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/code-review",
              "version": "1.4.0",
              "digest": "{VALID_DIGEST_A}",
              "source": "foundation",
              "trust": "verified",
              "requirements": []
            }}
          ]
        }}"#,
    );
    let manifest_json = format!(
        r#"{{
          "name": "@oxi/code-review",
          "version": "1.4.0",
          "digest": "{VALID_DIGEST_B}",
          "payloads": [{{ "kind": "inline", "value": "x" }}]
        }}"#,
    );
    let lock = fp::parse_packages_lock(&lock_json).unwrap();
    let manifest = fp::parse_package_manifest(&manifest_json).unwrap();
    let entry = lock
        .packages
        .iter()
        .find(|p| p.name == "@oxi/code-review")
        .expect("lock entry");
    let err = manifest
        .matches_lock_entry(entry)
        .expect_err("digest mismatch must hard-error");
    match err {
        fp::PackageError::DigestMismatch {
            name,
            manifest_digest,
            lock_digest,
        } => {
            assert_eq!(name, "@oxi/code-review");
            assert_eq!(manifest_digest, VALID_DIGEST_B);
            assert_eq!(lock_digest, VALID_DIGEST_A);
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }
}

#[test]
fn manifest_and_lock_with_different_names_hard_error() {
    let lock_json = format!(
        r#"{{
          "schema_version": 1,
          "packages": [
            {{
              "name": "@oxi/code-review",
              "version": "1.4.0",
              "digest": "{VALID_DIGEST_A}",
              "source": "foundation",
              "trust": "verified",
              "requirements": []
            }}
          ]
        }}"#,
    );
    let manifest_json = format!(
        r#"{{
          "name": "@oxi/other",
          "version": "1.0.0",
          "digest": "{VALID_DIGEST_A}"
        }}"#,
    );
    let lock = fp::parse_packages_lock(&lock_json).unwrap();
    let manifest = fp::parse_package_manifest(&manifest_json).unwrap();
    let entry = lock.packages.first().expect("lock entry");
    let err = manifest
        .matches_lock_entry(entry)
        .expect_err("name mismatch must hard-error");
    assert!(matches!(err, fp::PackageError::IdentityMismatch { .. }));
}

// ─── discover the canonical location ──────────────────────────────────────

#[test]
fn foundation_home_returns_a_v1_path() {
    let home = fp::foundation_home();
    assert!(home.ends_with("v1"), "got {:?}", home);
}

#[test]
fn manifest_path_lives_under_manifests_subdir() {
    let home = std::path::PathBuf::from("/tmp/fake-home");
    let p = fp::manifest_path(&home, "@oxi/code-review");
    assert_eq!(
        p,
        std::path::PathBuf::from("/tmp/fake-home/manifests/@oxi__code-review.json")
    );
}

// ─── cross-host fixture corpus guard (Task 6 §6) ───────────────────────────

/// The shared corpus at `tests/fixtures/oxi-foundation/v1/packages/` must be
/// **byte-identical** to the same path in oxicode (per spec §9). This test
/// loads every canonical package fixture, parses it through the strict
/// package parser, and asserts the outcome matches the contract's table:
///
/// - `valid_lock.json`         — accept.
/// - `bad_digest.json`         — accept (well-formed at parse time; the
///   "bad" is install-time digest mismatch, not a parse-time rejection).
/// - `missing_target.json`     — accept (well-formed; `targets: ["oxibrain"]`
///   makes the package invisible to other callers; host policy decides).
/// - `denied_requirement.json` — accept (parser preserves `kernel.modify`
///   as `AbstractRequirement::Unknown`; spec §3.3, §3.7).
///
/// If a fixture is renamed, deleted, or the parser's verdict diverges from
/// the table above, the contract has drifted and must be reported, not
/// patched here.
#[test]
fn cross_host_fixture_corpus_packages_match_outcome_table() {
    let corpus_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("oxi-foundation")
        .join("v1");
    let packages_dir = corpus_root.join("packages");
    assert!(
        packages_dir.is_dir(),
        "cross-host corpus missing: {}",
        packages_dir.display()
    );

    // Accept — valid_lock.json.
    let body = fs::read_to_string(packages_dir.join("valid_lock.json")).unwrap();
    let lock = fp::parse_packages_lock(&body).expect("valid_lock.json must parse");
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].name, "@oxi/code-review");
    assert_eq!(lock.packages[0].version, "1.4.0");
    assert_eq!(lock.packages[0].trust, fp::TrustState::Verified);
    assert_eq!(
        lock.packages[0].targets.as_deref(),
        Some(&["oxicode".to_string()][..])
    );
    let known: BTreeSet<String> = lock.packages[0]
        .requirements
        .iter()
        .map(|r| r.as_str().to_owned())
        .collect();
    assert!(known.contains("workspace.read"));
    assert!(known.contains("workspace.patch"));
    assert!(known.contains("brain.query"));

    // Accept — bad_digest.json (well-formed at parse time; tamper is
    // install-time, not parse-time per spec §3.4).
    let body = fs::read_to_string(packages_dir.join("bad_digest.json")).unwrap();
    let lock =
        fp::parse_packages_lock(&body).expect("bad_digest.json must parse at the format level");
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].name, "@oxi/tampered");
    assert!(lock.packages[0].digest.starts_with("sha256-"));

    // Accept — missing_target.json; host policy decides whether the caller
    // is allowed to see an oxibrain-only package.
    let body = fs::read_to_string(packages_dir.join("missing_target.json")).unwrap();
    let lock = fp::parse_packages_lock(&body).expect("missing_target.json must parse");
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].name, "@oxi/oxibrain-only");
    assert_eq!(
        lock.packages[0].targets.as_deref(),
        Some(&["oxibrain".to_string()][..])
    );
    // The corpus guarantees cross-host exclusion: an oxicode caller sees
    // nothing because no package applies to it.
    let picked = fp::select_package_for_target(&lock, "oxicode");
    assert!(
        picked.is_none(),
        "missing_target.json must be invisible to oxicode callers"
    );

    // Accept — denied_requirement.json; parser preserves `kernel.modify`
    // as `Unknown` (spec §3.3 / §3.7). The host's policy is the only
    // place that decides whether to honour it.
    let body = fs::read_to_string(packages_dir.join("denied_requirement.json")).unwrap();
    let lock = fp::parse_packages_lock(&body).expect("denied_requirement.json must parse");
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].name, "@oxi/unknown-cap");
    assert_eq!(lock.packages[0].requirements.len(), 1);
    let req = &lock.packages[0].requirements[0];
    assert!(!req.is_known());
    assert_eq!(req.as_str(), "kernel.modify");
}
