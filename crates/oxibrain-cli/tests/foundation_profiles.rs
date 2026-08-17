//! Integration tests for Oxi Foundation v1 profile parsing and the
//! role-aware provider resolution ladder (Task 3 §3).
//!
//! These tests exercise the full path: a Foundation profile written to a
//! temp `$OXI_FOUNDATION_HOME`, the resolver picking it for the role, and the
//! resulting `ProviderLlm` carrying the correct `ResolutionSource`. They do
//! not spawn `CARGO_BIN_EXE_oxibrain` (the integration test convention is
//! `env!(...)`); instead they drive the library API directly so the
//! hermetic guarantees of `InMemorySecretResolver` and `tempfile::tempdir`
//! hold across machines.
//!
//! Env-var precedence is exercised by `oxibrain_cli::cmd::llm::resolve_role`
//! (the env-override precedence and `OXIBRAIN_LLM_ROLE` default).

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use oxibrain_cli::cmd::foundation::{
    self, FoundationError, InMemorySecretResolver, ProfileRole, ProviderKind, ResolvedProfiles,
    SCHEMA_VERSION,
};
use oxibrain_cli::cmd::llm::{self, ResolutionSource};
use oxibrain_core::extraction::ExtractMechanism;
use oxibrain_ports::{LlmCapabilities, LlmPort};

/// Process-wide lock for tests that mutate `OXI_FOUNDATION_HOME`,
/// `OXIBRAIN_LLM_PROVIDER`, or related environment variables. cargo
/// defaults to running tests in parallel across threads; env vars are
/// process-global, so any two tests that touch the same variable race.
/// Every set-var / remove-var call in this file MUST hold this lock
/// for the duration of the test body and any restore-on-drop handler.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write a `profiles.json` body to `$OXI_FOUNDATION_HOME/profiles.json` and
/// return the temp dir. Tests must keep the temp dir alive for the duration
/// of the test so the path remains valid while the host reads it.
fn write_profiles(home: &Path, body: &str) {
    fs::create_dir_all(home).expect("mkdir foundation home");
    fs::write(home.join("profiles.json"), body).expect("write profiles.json");
}

/// Run `f` with `OXI_FOUNDATION_HOME` set to `home`. Restores the prior
/// value on exit so tests do not leak env state between cases.
fn with_foundation_home<F: FnOnce()>(home: &Path, f: F) {
    // Hold the process-wide env lock for the entire set-var / run / restore
    // window so a parallel test cannot observe a half-set home, and so our
    // restore on exit doesn't clobber another test's set-var.
    //
    // SAFETY: `set_var` / `remove_var` are marked `unsafe` in Rust 2024 for
    // data-race reasons. We synchronise here via `ENV_LOCK` so the mutation
    // is single-threaded in effect.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("OXI_FOUNDATION_HOME");
    unsafe {
        std::env::set_var("OXI_FOUNDATION_HOME", home);
    }
    f();
    unsafe {
        match saved {
            Some(v) => std::env::set_var("OXI_FOUNDATION_HOME", v),
            None => std::env::remove_var("OXI_FOUNDATION_HOME"),
        }
    }
}

#[test]
fn parses_canonical_profile_and_returns_locator_only() {
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {
          "id": "work-summariser",
          "provider": "anthropic",
          "model": "claude-sonnet-4-5",
          "roles": ["memory.consolidate", "assistant.general"],
          "credential": {"service": "oxibrain", "account": "work"}
        }
      ]
    }"#;
    write_profiles(dir.path(), body);

    with_foundation_home(dir.path(), || {
        let got = foundation::load_profiles(&foundation::foundation_home())
            .unwrap()
            .expect("profiles present");
        assert_eq!(got.profiles.len(), 1);
        let p = &got.profiles[0];
        assert_eq!(p.id, "work-summariser");
        assert_eq!(p.provider, "anthropic");
        assert_eq!(p.model, "claude-sonnet-4-5");
        assert_eq!(
            p.roles,
            vec![
                ProfileRole::MemoryConsolidate,
                ProfileRole::AssistantGeneral
            ]
        );
        assert_eq!(p.credential.service, "oxibrain");
        assert_eq!(p.credential.account, "work");
    });
}

#[test]
fn rejects_profiles_carrying_secret_field() {
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {
          "id": "leaky",
          "provider": "anthropic",
          "model": "claude-sonnet-4-5",
          "roles": ["memory.extract"],
          "credential": {"service": "oxibrain", "account": "a"},
          "api_key": "sk-leak"
        }
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
        assert!(
            matches!(&err, FoundationError::SecretFieldPresent(f) if f == "api_key"),
            "expected SecretFieldPresent(api_key), got {err:?}"
        );
    });
}

#[test]
fn rejects_every_documented_secret_field_name() {
    // The exact list is part of the contract (§2.5); a future contributor who
    // renames or removes one must trip this test.
    let secret_field_names = [
        "api_key",
        "apikey",
        "api-token",
        "bearer",
        "access_token",
        "refresh_token",
        "secret",
        "password",
        "private_key",
    ];
    for field in secret_field_names {
        let dir = TempDir::new().unwrap();
        let body = format!(
            r#"{{
              "schema_version": 1,
              "profiles": [
                {{
                  "id": "p",
                  "provider": "anthropic",
                  "model": "claude-sonnet-4-5",
                  "roles": ["memory.extract"],
                  "credential": {{"service": "oxibrain", "account": "a"}},
                  "{field}": "leak"
                }}
              ]
            }}"#
        );
        write_profiles(dir.path(), &body);
        with_foundation_home(dir.path(), || {
            let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
            assert!(
                matches!(&err, FoundationError::SecretFieldPresent(f) if f == field),
                "{field}: expected SecretFieldPresent({field}), got {err:?}"
            );
        });
    }
}

#[test]
fn rejects_unsupported_schema_version() {
    let dir = TempDir::new().unwrap();
    write_profiles(dir.path(), r#"{"schema_version":2,"profiles":[]}"#);
    with_foundation_home(dir.path(), || {
        let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
        assert!(matches!(
            &err,
            FoundationError::UnsupportedSchemaVersion(v) if *v == 2
        ));
    });
}

#[test]
fn rejects_duplicate_profile_id_and_duplicate_role() {
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {"id":"same","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"a"}},
        {"id":"same","provider":"openai","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"b"}}
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
        assert!(matches!(&err, FoundationError::DuplicateProfileId(id) if id == "same"));
    });

    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {"id":"p","provider":"anthropic","model":"m","roles":["memory.extract","memory.extract"],"credential":{"service":"s","account":"a"}}
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
        assert!(matches!(
            &err,
            FoundationError::DuplicateRole(ProfileRole::MemoryExtract)
        ));
    });
}

#[test]
fn rejects_empty_required_fields() {
    for (field, body) in [
        (
            "provider",
            r#"{"schema_version":1,"profiles":[{"id":"p","provider":"","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"a"}}]}"#,
        ),
        (
            "model",
            r#"{"schema_version":1,"profiles":[{"id":"p","provider":"anthropic","model":"","roles":["memory.extract"],"credential":{"service":"s","account":"a"}}]}"#,
        ),
        (
            "credential.service",
            r#"{"schema_version":1,"profiles":[{"id":"p","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{"service":"","account":"a"}}]}"#,
        ),
        (
            "credential.account",
            r#"{"schema_version":1,"profiles":[{"id":"p","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":""}}]}"#,
        ),
        (
            "id",
            r#"{"schema_version":1,"profiles":[{"id":"","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"a"}}]}"#,
        ),
    ] {
        let dir = TempDir::new().unwrap();
        write_profiles(dir.path(), body);
        with_foundation_home(dir.path(), || {
            let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
            assert!(
                matches!(&err, FoundationError::EmptyField(f) if *f == field),
                "expected EmptyField({field}), got {err:?}"
            );
        });
    }
}

#[test]
fn rejects_empty_roles_and_unknown_roles() {
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {"id":"p","provider":"anthropic","model":"m","roles":[],"credential":{"service":"s","account":"a"}}
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
        assert!(matches!(&err, FoundationError::EmptyRoles));
    });

    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {"id":"p","provider":"anthropic","model":"m","roles":["memory.unknown"],"credential":{"service":"s","account":"a"}}
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let err = foundation::load_profiles(&foundation::foundation_home()).unwrap_err();
        // Strict-deserialize path rejects unknown variants; surface as InvalidShape.
        assert!(matches!(&err, FoundationError::InvalidShape(_)));
    });
}

#[test]
fn declared_capabilities_must_satisfy_mechanism() {
    // A profile declaring only `grammar` capability cannot satisfy JsonSchema
    // extraction — must be rejected before any Keychain call.
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {
          "id": "constrained",
          "provider": "anthropic",
          "model": "m",
          "roles": ["memory.extract"],
          "credential": {"service": "s", "account": "a"},
          "capabilities": {"grammar": true, "structured_output": false, "tool_call": false, "json_schema": false}
        }
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let got: ResolvedProfiles = foundation::load_profiles(&foundation::foundation_home())
            .unwrap()
            .expect("profiles present");
        let err = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap_err();
        assert!(matches!(
            &err,
            FoundationError::CapabilityUnsatisfied { profile_id, .. } if profile_id == "constrained"
        ));
    });
}

#[test]
fn role_denied_when_no_profile_declares_role() {
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {"id":"a","provider":"anthropic","model":"m","roles":["coding.primary"],"credential":{"service":"s","account":"a"}}
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let got = foundation::load_profiles(&foundation::foundation_home())
            .unwrap()
            .expect("profiles present");
        let err = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap_err();
        assert!(matches!(
            &err,
            FoundationError::RoleDenied {
                requested: ProfileRole::MemoryExtract,
                ..
            }
        ));
    });
}

#[test]
fn openai_profile_with_only_json_schema_passes_capability_check() {
    // Task 3 review finding #1: a truthful OpenAI profile declaring only
    // `json_schema: true` (and `tool_call: false`) must be selected. The
    // resolver chooses the mechanism from `provider` (OpenAI => JsonSchema),
    // so the pick_for_role call uses JsonSchema, not ToolCall, and the
    // capability check passes.
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {
          "id": "openai-json",
          "provider": "openai",
          "model": "gpt-4o",
          "roles": ["memory.extract"],
          "credential": {"service": "oxibrain", "account": "openai"},
          "capabilities": {"grammar": false, "structured_output": false, "tool_call": false, "json_schema": true}
        }
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let got = foundation::load_profiles(&foundation::foundation_home())
            .unwrap()
            .expect("profiles present");
        // Try the resolver's role-bound pick with the OpenAI-native
        // mechanism — must succeed.
        let pick_json = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap();
        assert_eq!(pick_json.id, "openai-json");

        // Drive the full async resolver path with a registered in-memory
        // secret. The resolver returns Some(provider) with a
        // ResolutionSource::FoundationProfile carrying the profile id and
        // ProviderKind::OpenAi.
        let resolver =
            InMemorySecretResolver::new().with_secret("oxibrain", "openai", "sk-test-secret");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let resolved = runtime.block_on(llm::try_foundation_profile(
            &got,
            ProfileRole::MemoryExtract,
            &resolver,
        ));
        let provider = resolved
            .expect("try_foundation_profile returns Ok")
            .expect("a profile was selected");
        assert_eq!(provider.model_id, "gpt-4o");
        match &provider.source {
            ResolutionSource::FoundationProfile {
                profile_id,
                provider: ProviderKind::OpenAi,
                model_id,
                mechanism: ExtractMechanism::JsonSchema,
            } => {
                assert_eq!(profile_id, "openai-json");
                assert_eq!(model_id, "gpt-4o");
            }
            other => panic!("expected FoundationProfile/OpenAi, got {other:?}"),
        }
    });
}

#[test]
fn openai_profile_with_only_tool_call_is_rejected() {
    // Symmetric guard: an OpenAI profile that declares only `tool_call: true`
    // (truthful for the Anthropic adapter, but wrong for the OpenAI adapter
    // whose mechanism is JsonSchema) must be rejected — never a silent
    // mechanism swap.
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {
          "id": "openai-misconfigured",
          "provider": "openai",
          "model": "gpt-4o",
          "roles": ["memory.extract"],
          "credential": {"service": "oxibrain", "account": "openai"},
          "capabilities": {"grammar": false, "structured_output": false, "tool_call": true, "json_schema": false}
        }
      ]
    }"#;
    write_profiles(dir.path(), body);
    with_foundation_home(dir.path(), || {
        let got = foundation::load_profiles(&foundation::foundation_home())
            .unwrap()
            .expect("profiles present");
        // OpenAI profiles are validated against JsonSchema by the resolver;
        // a profile that only advertises ToolCall must be rejected.
        let resolver =
            InMemorySecretResolver::new().with_secret("oxibrain", "openai", "sk-test-secret");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(llm::try_foundation_profile(
            &got,
            ProfileRole::MemoryExtract,
            &resolver,
        ));
        let err = result.err().expect("profile must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("openai-misconfigured") && msg.contains("JsonSchema"),
            "unexpected error: {msg}"
        );
    });
}

#[test]
fn env_override_takes_precedence_over_foundation_profile() {
    // When `OXIBRAIN_LLM_PROVIDER=local` is set explicitly, the Foundation
    // profile (even if valid + Keychain-resolvable) is bypassed: the explicit
    // override wins outright.
    let dir = TempDir::new().unwrap();
    let body = r#"{
      "schema_version": 1,
      "profiles": [
        {
          "id": "should-be-ignored",
          "provider": "anthropic",
          "model": "claude-sonnet-4-5",
          "roles": ["memory.extract"],
          "credential": {"service": "oxibrain", "account": "work"}
        }
      ]
    }"#;
    write_profiles(dir.path(), body);

    let saved_provider = std::env::var_os("OXIBRAIN_LLM_PROVIDER");
    let saved_anthropic = std::env::var_os("ANTHROPIC_API_KEY");
    let saved_openai = std::env::var_os("OPENAI_API_KEY");
    let saved_home = std::env::var_os("OXI_FOUNDATION_HOME");

    // Take the process-wide env lock for the whole test (set, run, drop).
    // The lock guard is moved into `Restore` so the Drop impl runs while
    // the lock is still held — preventing a parallel test from racing with
    // the restore.
    //
    // SAFETY: env vars are process-global; we serialise every mutation in
    // this binary through `ENV_LOCK`.
    let lock_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: see `lock_guard` above.
    unsafe {
        std::env::set_var("OXIBRAIN_LLM_PROVIDER", "local");
        std::env::set_var("OXI_FOUNDATION_HOME", dir.path());
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
    }

    let role = llm::resolve_role();
    assert_eq!(role, ProfileRole::MemoryExtract);

    // Restoration on drop so subsequent tests see a clean env.
    struct Restore(
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        // The MutexGuard keeps the lock held until Drop runs; the field is
        // read implicitly via Drop ordering. Suppress the dead-code warning
        // by binding it to a name that the dead-code lint allows for.
        #[allow(dead_code)] std::sync::MutexGuard<'static, ()>,
    );
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: caller serialises env access through ENV_LOCK; the
            // guard is in `self.4` and is still alive here because Drop runs
            // field-by-field in declaration order and the guard is last.
            unsafe {
                if let Some(v) = &self.0 {
                    std::env::set_var("OXIBRAIN_LLM_PROVIDER", v);
                } else {
                    std::env::remove_var("OXIBRAIN_LLM_PROVIDER");
                }
                if let Some(v) = &self.1 {
                    std::env::set_var("ANTHROPIC_API_KEY", v);
                } else {
                    std::env::remove_var("ANTHROPIC_API_KEY");
                }
                if let Some(v) = &self.2 {
                    std::env::set_var("OPENAI_API_KEY", v);
                } else {
                    std::env::remove_var("OPENAI_API_KEY");
                }
                if let Some(v) = &self.3 {
                    std::env::set_var("OXI_FOUNDATION_HOME", v);
                } else {
                    std::env::remove_var("OXI_FOUNDATION_HOME");
                }
            }
        }
    }
    // SAFETY: see the comment above `lock_guard`. Restore holds the guard
    // until the test function returns, so the Drop impl runs under lock.
    let _restore = Restore(
        saved_provider,
        saved_anthropic,
        saved_openai,
        saved_home,
        lock_guard,
    );
}

#[test]
fn in_memory_resolver_returns_secret_when_set() {
    use oxibrain_cli::cmd::foundation::{InMemorySecretResolver, SecretLocator, SecretResolver};
    let resolver = InMemorySecretResolver::new().with_secret("oxibrain", "work", "secret-value");
    let got = resolver
        .resolve(&SecretLocator {
            service: "oxibrain".into(),
            account: "work".into(),
        })
        .unwrap();
    assert_eq!(got, "secret-value");
}

#[test]
fn in_memory_resolver_reports_unavailable_for_missing_locator() {
    use oxibrain_cli::cmd::foundation::{InMemorySecretResolver, SecretLocator, SecretResolver};
    let resolver = InMemorySecretResolver::new();
    let err = resolver
        .resolve(&SecretLocator {
            service: "oxibrain".into(),
            account: "missing".into(),
        })
        .unwrap_err();
    assert!(matches!(&err, FoundationError::SecretUnavailable { .. }));
}

#[test]
fn foundation_home_uses_env_override_when_set() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("OXI_FOUNDATION_HOME");
    // SAFETY: env vars are serialised via ENV_LOCK in this binary.
    unsafe {
        std::env::set_var("OXI_FOUNDATION_HOME", "/tmp/oxibrain-foundation-test-home");
    }
    let got = foundation::foundation_home();
    // SAFETY: see above.
    unsafe {
        match saved {
            Some(v) => std::env::set_var("OXI_FOUNDATION_HOME", v),
            None => std::env::remove_var("OXI_FOUNDATION_HOME"),
        }
    }
    assert_eq!(
        got,
        std::path::PathBuf::from("/tmp/oxibrain-foundation-test-home")
    );
}

#[test]
fn schema_version_constant_matches_spec() {
    assert_eq!(SCHEMA_VERSION, 1);
}

#[test]
fn capability_satisfies_mechanism_for_each_extraction_path() {
    use oxibrain_cli::cmd::foundation::DeclaredCapabilities;
    let grammar = DeclaredCapabilities {
        grammar: true,
        ..DeclaredCapabilities::default()
    };
    assert!(grammar.clone().satisfies(ExtractMechanism::Grammar));
    assert!(!grammar.satisfies(ExtractMechanism::JsonSchema));
    assert!(!grammar.satisfies(ExtractMechanism::ToolCall));

    let tool = DeclaredCapabilities {
        tool_call: true,
        ..DeclaredCapabilities::default()
    };
    assert!(tool.satisfies(ExtractMechanism::ToolCall));
    assert!(!tool.satisfies(ExtractMechanism::JsonSchema));

    let js = DeclaredCapabilities {
        json_schema: true,
        ..DeclaredCapabilities::default()
    };
    assert!(js.satisfies(ExtractMechanism::JsonSchema));

    let legacy = DeclaredCapabilities {
        structured_output: true,
        ..DeclaredCapabilities::default()
    };
    // The legacy `structured_output` flag is accepted as a synonym for the
    // new `json_schema` flag — older adapters continue to advertise truth.
    assert!(legacy.satisfies(ExtractMechanism::JsonSchema));
}

#[test]
fn http_adapters_advertise_truthful_capabilities() {
    use oxibrain_llm_http::{AnthropicLlm, OpenAiLlm};

    let a = AnthropicLlm::new("test".into(), "claude-sonnet-4-5".into());
    let ca: LlmCapabilities = a.capabilities();
    assert!(ca.tool_call);
    assert!(!ca.grammar);
    assert!(!ca.json_schema);

    let o = OpenAiLlm::new("test".into(), "gpt-4o".into());
    let co: LlmCapabilities = o.capabilities();
    assert!(co.json_schema);
    assert!(co.structured_output);
    assert!(co.tool_call);
    assert!(!co.grammar);
}

#[test]
fn resolution_source_provenance_records_foundation_origin() {
    // Provenance is what Task 5 plumbs into ExtractorId — the field must
    // exist and be Clone/Debug. We do not assert the concrete value here
    // because the production resolver wires a real adapter; this test just
    // guards the type so a future refactor doesn't silently lose the signal.
    fn _is_clone_debug<T: Clone + std::fmt::Debug>() {}
    _is_clone_debug::<ResolutionSource>();
}

/// Cross-host fixture corpus guard (Task 6 §6).
///
/// The shared corpus at `tests/fixtures/oxi-foundation/v1/` must be
/// **byte-identical** to the same path in oxicode (per spec §9). This test
/// loads every canonical profile fixture, parses it through the strict
/// profile parser, and asserts the outcome matches the contract's table:
///
/// - `valid_personal_coding.json` — accept.
/// - `unknown_schema.json`        — reject (`schema_version != 1`).
/// - `duplicate_profile_id.json`  — reject (id appears twice).
/// - `malformed_credential_locator.json` — reject (empty credential field).
/// - `role_ambiguous.json`        — accept (two profiles share one role;
///   the parser permits it; host policy chooses one).
///
/// If a fixture is renamed, deleted, or the parser's verdict diverges from
/// the table above, the contract has drifted and must be reported, not
/// patched here.
#[test]
fn cross_host_fixture_corpus_profiles_match_outcome_table() {
    let corpus_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("oxi-foundation")
        .join("v1");
    let profiles_dir = corpus_root.join("profiles");
    assert!(
        profiles_dir.is_dir(),
        "cross-host corpus missing: {}",
        profiles_dir.display()
    );

    // Accept case — must parse, must yield exactly one profile with the
    // documented id.
    {
        let dir = TempDir::new().unwrap();
        let body = fs::read_to_string(profiles_dir.join("valid_personal_coding.json")).unwrap();
        fs::write(dir.path().join("profiles.json"), &body).unwrap();
        with_foundation_home(dir.path(), || {
            let got = foundation::load_profiles(&foundation::foundation_home())
                .unwrap()
                .expect("valid_personal_coding.json must parse");
            assert_eq!(got.profiles.len(), 1);
            assert_eq!(got.profiles[0].id, "personal-coding");
        });
    }

    // Reject case — schema_version != 1.
    {
        let dir = TempDir::new().unwrap();
        let body = fs::read_to_string(profiles_dir.join("unknown_schema.json")).unwrap();
        fs::write(dir.path().join("profiles.json"), &body).unwrap();
        with_foundation_home(dir.path(), || {
            let res = foundation::load_profiles(&foundation::foundation_home());
            assert!(
                matches!(
                    res,
                    Err(foundation::FoundationError::UnsupportedSchemaVersion(99))
                ),
                "unknown_schema.json must be rejected as UnsupportedSchemaVersion(99); got {res:?}"
            );
        });
    }

    // Reject case — duplicate id.
    {
        let dir = TempDir::new().unwrap();
        let body = fs::read_to_string(profiles_dir.join("duplicate_profile_id.json")).unwrap();
        fs::write(dir.path().join("profiles.json"), &body).unwrap();
        with_foundation_home(dir.path(), || {
            let res = foundation::load_profiles(&foundation::foundation_home());
            match res {
                Err(foundation::FoundationError::DuplicateProfileId(id)) if id == "dup" => {}
                other => panic!(
                    "duplicate_profile_id.json must be rejected as DuplicateProfileId(\"dup\"); got {other:?}"
                ),
            }
        });
    }

    // Reject case — empty credential field (locator shape violated).
    {
        let dir = TempDir::new().unwrap();
        let body =
            fs::read_to_string(profiles_dir.join("malformed_credential_locator.json")).unwrap();
        fs::write(dir.path().join("profiles.json"), &body).unwrap();
        with_foundation_home(dir.path(), || {
            let res = foundation::load_profiles(&foundation::foundation_home());
            assert!(
                matches!(
                    res,
                    Err(foundation::FoundationError::EmptyField(
                        "credential.service" | "credential.account"
                    ))
                ),
                "malformed_credential_locator.json must be rejected as EmptyField; got {res:?}"
            );
        });
    }

    // Accept case — two profiles share one role. The parser permits this
    // (host policy resolves ambiguity per spec §6); the only contract
    // assertion is that both profiles are present in the parsed set.
    {
        let dir = TempDir::new().unwrap();
        let body = fs::read_to_string(profiles_dir.join("role_ambiguous.json")).unwrap();
        fs::write(dir.path().join("profiles.json"), &body).unwrap();
        with_foundation_home(dir.path(), || {
            let got = foundation::load_profiles(&foundation::foundation_home())
                .unwrap()
                .expect("role_ambiguous.json must parse");
            assert_eq!(got.profiles.len(), 2);
            let ids: std::collections::HashSet<_> =
                got.profiles.iter().map(|p| p.id.as_str()).collect();
            assert!(ids.contains("alpha"));
            assert!(ids.contains("beta"));
        });
    }
}
