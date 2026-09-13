//! End-to-end tests for the document plane sequencer (Task 6).
//!
//! All tests use a tempdir as the brain dir and a plain (non-git) root on
//! disk so we never depend on the gix exclude platform or an index file.
//! The git-root tests live in `git_root.rs` (Task 6 also covers them).
//!
//! Conventions:
//! - Timestamps use ms-precision real time; ordering across tests does not
//!   matter because we never span tests.
//! - `Brain` returns `CaptureOutcome::CapturedPending` when extraction is
//!   configured with a failing LLM; that is the headline scenario the plan
//!   lists for the LLM-outside-tx invariant.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use oxibrain::config::BrainConfig;
use oxibrain::document_plane::{DocumentFreshness, IndexOptions, SearchResponse};
use oxibrain::{Brain, CaptureOutcome};
use oxibrain_ports::{BrainError, ClockPort, LlmPort, LlmRequest, LlmResponse, Timestamp};
use tempfile::TempDir;

#[derive(Debug, Clone)]
struct FakeClock(Timestamp);

impl ClockPort for FakeClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[derive(Debug)]
struct AlwaysFail;

#[async_trait::async_trait]
impl LlmPort for AlwaysFail {
    async fn complete(&self, _: LlmRequest) -> Result<LlmResponse, BrainError> {
        Err(BrainError::Model("synthetic failure".into()))
    }
    async fn generate_constrained(
        &self,
        _: LlmRequest,
        _grammar: &str,
    ) -> Result<LlmResponse, BrainError> {
        Err(BrainError::Model("synthetic failure".into()))
    }
    fn capabilities(&self) -> oxibrain_ports::LlmCapabilities {
        oxibrain_ports::LlmCapabilities::default()
    }
}

/// Returns a syntactically invalid extraction response — the parse-failure
/// path, which records an `extraction_failures` row.
#[derive(Debug)]
struct NotJson;

#[async_trait::async_trait]
impl LlmPort for NotJson {
    async fn complete(&self, _: LlmRequest) -> Result<LlmResponse, BrainError> {
        Ok(LlmResponse {
            text: "{not json".into(),
            raw: serde_json::Value::Null,
        })
    }
    async fn generate_constrained(
        &self,
        _: LlmRequest,
        _grammar: &str,
    ) -> Result<LlmResponse, BrainError> {
        Ok(LlmResponse {
            text: "{not json".into(),
            raw: serde_json::Value::Null,
        })
    }
    fn capabilities(&self) -> oxibrain_ports::LlmCapabilities {
        oxibrain_ports::LlmCapabilities::default()
    }
}

fn write_file(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, body).unwrap();
}

async fn make_brain(dir: &Path) -> Brain {
    let clock = Arc::new(FakeClock(Timestamp::from_millis(1_700_000_000_000)));
    Brain::with_clock(BrainConfig::at(dir.to_str().unwrap()), clock)
        .await
        .unwrap()
}

async fn make_brain_with_failing_llm(dir: &Path) -> Brain {
    let clock = Arc::new(FakeClock(Timestamp::from_millis(1_700_000_000_000)));
    let llm: Arc<dyn LlmPort> = Arc::new(AlwaysFail);
    Brain::with_llm(BrainConfig::at(dir.to_str().unwrap()), clock, llm)
        .await
        .unwrap()
}

fn write_documents_config(root_dir: &Path, vault_dir: &Path, space: &str) {
    let toml = format!(
        "[[root]]\nalias = \"vault\"\npath = \"{}\"\nspace = \"{}\"\n",
        vault_dir.display(),
        space,
    );
    fs::write(root_dir.join("documents.toml"), toml).unwrap();
}

#[tokio::test]
async fn search_documents_returns_verbatim_slice_with_revision() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "alpha/notes.md", "alpha content");
    write_file(vault.path(), "beta/notes.md", "beta content");

    let brain = make_brain(brain_dir.path()).await;
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    assert!(
        freshness.reconciled_roots.iter().any(|a| a == "vault"),
        "expected vault root reconciled, got {freshness:?}"
    );

    let q = oxibrain_core::retrieval::Query {
        text: "alpha".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [oxibrain_core::retrieval::SearchPlane::Documents]
            .into_iter()
            .collect(),
    };
    let response = brain.search(q).await.unwrap();
    assert!(
        !response.documents.is_empty(),
        "expected at least one document hit, response: {response:?}",
    );
    let docs = &response.documents;
    let hit = &docs[0];
    assert_eq!(hit.root, "vault");
    assert_eq!(hit.locator, "alpha/notes.md");
    assert!(hit.text.text.contains("alpha content"));
    assert!(!hit.revision.is_empty());
}

#[tokio::test]
async fn search_documents_detects_in_place_edit() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "edit/notes.md", "first version");

    let brain = make_brain(brain_dir.path()).await;
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    // In-place edit (no locator change). Force an mtime advance by
    // rewriting the file with a content change.
    write_file(vault.path(), "edit/notes.md", "second version");
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    let q = oxibrain_core::retrieval::Query {
        text: "second".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [oxibrain_core::retrieval::SearchPlane::Documents]
            .into_iter()
            .collect(),
    };
    let response = brain.search(q).await.unwrap();
    assert!(
        response
            .documents
            .iter()
            .any(|d| d.text.text.contains("second version")),
        "expected fresh edit to be visible: {:?}",
        response.documents
    );
}

#[tokio::test]
async fn deletion_completeness() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "del/notes.md", "to be deleted");

    let brain = make_brain(brain_dir.path()).await;
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    fs::remove_file(vault.path().join("del/notes.md")).unwrap();
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    let q = oxibrain_core::retrieval::Query {
        text: "deleted".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [oxibrain_core::retrieval::SearchPlane::Documents]
            .into_iter()
            .collect(),
    };
    let response = brain.search(q).await.unwrap();
    assert!(
        response.documents.is_empty(),
        "expected deletion to remove hits: {:?}",
        response.documents
    );
}

#[tokio::test]
async fn root_removal_clears_rows() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "gone.md", "bye");

    let brain = make_brain(brain_dir.path()).await;
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    // Drop the alias from the config so the next index rebuilds it away.
    fs::write(brain_dir.path().join("documents.toml"), "").unwrap();
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    let q = oxibrain_core::retrieval::Query {
        text: "bye".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [oxibrain_core::retrieval::SearchPlane::Documents]
            .into_iter()
            .collect(),
    };
    let response = brain.search(q).await.unwrap();
    assert!(
        response.documents.is_empty(),
        "expected removal to wipe rows, got {:?}",
        response.documents
    );
}

#[tokio::test]
async fn concurrent_index_documents_succeed_serially() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "one.md", "alpha");
    write_file(vault.path(), "two.md", "beta");

    let brain_a = make_brain(brain_dir.path()).await;
    let brain_b = make_brain(brain_dir.path()).await;

    let opts = IndexOptions {
        embed: false,
        budget: None,
    };
    let (a, b) = tokio::join!(
        brain_a.index_documents(opts.clone()),
        brain_b.index_documents(opts),
    );
    a.expect("first index");
    b.expect("second index");
}

#[tokio::test]
async fn remember_with_failing_llm_yields_captured_pending_and_recovers() {
    let brain_dir = TempDir::new().unwrap();
    let brain = make_brain_with_failing_llm(brain_dir.path()).await;
    let _space = brain.ensure_space("personal").await.unwrap();

    let outcome: CaptureOutcome = brain
        .remember(
            "personal",
            "notes.md".into(),
            "Alice works at Acme".into(),
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .expect("remember should not error even with failing LLM");
    match outcome {
        CaptureOutcome::CapturedPending {
            episode_id,
            pending,
        } => {
            assert!(!episode_id.is_empty());
            assert!(pending >= 1);
        }
        CaptureOutcome::Captured {
            episode_id,
            extracted,
        } => {
            panic!("unexpectedly got Captured with {extracted} extracted for {episode_id}");
        }
    }

    let stats = brain.pending_extraction_stats().await.unwrap();
    assert!(stats.count >= 1);

    // The backlog remains until extract_uncached runs.
    let recovered = brain.extract_uncached(10).await.unwrap();
    assert!(
        recovered >= 1,
        "extract_uncached should report >= 1 processed episode"
    );
}

#[tokio::test]
async fn legacy_document_episodes_excluded_from_memory_search() {
    let brain_dir = TempDir::new().unwrap();
    let brain = make_brain(brain_dir.path()).await;
    let space = brain.ensure_space("personal").await.unwrap();
    // Ingest with SourceRef::Document (legacy kind). It must be excluded from
    // the memory plane search results.
    let ep = brain
        .ingest_event(
            &space,
            "legacy document body".into(),
            oxibrain_core::SourceRef::Document {
                uri: "doc://legacy".into(),
            },
            oxibrain_core::TrustTier::Trusted,
            None,
            "test",
        )
        .await
        .unwrap();
    assert!(!ep.is_empty());

    let q = oxibrain_core::retrieval::Query {
        text: "legacy".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [oxibrain_core::retrieval::SearchPlane::Memory]
            .into_iter()
            .collect(),
    };
    let response = brain.search(q).await.unwrap();
    assert!(
        response.memory.is_empty(),
        "memory plane must exclude legacy document episode: {:?}",
        response.memory
    );
}

#[tokio::test]
async fn pending_extraction_stats_reports_count() {
    let brain_dir = TempDir::new().unwrap();
    let brain = make_brain_with_failing_llm(brain_dir.path()).await;
    brain.ensure_space("personal").await.unwrap();
    brain
        .remember(
            "personal",
            "a.md".into(),
            "first".into(),
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .unwrap();
    brain
        .remember(
            "personal",
            "b.md".into(),
            "second".into(),
            Timestamp::from_millis(1_700_000_001_000),
        )
        .await
        .unwrap();
    let stats = brain.pending_extraction_stats().await.unwrap();
    assert_eq!(stats.count, 2);
}

/// A validation-poison episode (parse failure recorded at `now`) must not
/// re-run on the next drain: the retry cooldown hides it from both the
/// walker and `pending_extraction_stats`. 2026-09-01 oxios drain incident:
/// the poison re-ran a full generation every 30 min forever.
#[tokio::test]
async fn failed_extraction_enters_retry_cooldown() {
    let brain_dir = TempDir::new().unwrap();
    let clock = Arc::new(FakeClock(Timestamp::from_millis(1_700_000_000_000)));
    let llm: Arc<dyn LlmPort> = Arc::new(NotJson);
    let brain = Brain::with_llm(
        BrainConfig::at(brain_dir.path().to_str().unwrap()),
        clock,
        llm,
    )
    .await
    .unwrap();
    brain.ensure_space("personal").await.unwrap();
    let outcome: CaptureOutcome = brain
        .remember(
            "personal",
            "a.md".into(),
            "Alice works at Acme".into(),
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .unwrap();
    // Inline extraction failed on the garbage response, so the episode is
    // uncached — but the fresh failure puts it inside the cooldown.
    assert!(matches!(outcome, CaptureOutcome::CapturedPending { .. }));
    let stats = brain.pending_extraction_stats().await.unwrap();
    assert_eq!(stats.count, 0, "recent failure must be inside the cooldown");
    let processed = brain.extract_uncached(10).await.unwrap();
    assert_eq!(
        processed, 0,
        "drain must not re-attempt within the cooldown"
    );
}

#[tokio::test]
async fn index_documents_returns_freshness_summary() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    // One missing-on-disk root via a second alias.
    let cfg = fs::read_to_string(brain_dir.path().join("documents.toml")).unwrap();
    let extra = format!(
        "{}\n[[root]]\nalias = \"missing\"\npath = \"/this/does/not/exist/anywhere\"\nspace = \"personal\"\n",
        cfg
    );
    fs::write(brain_dir.path().join("documents.toml"), extra).unwrap();
    write_file(vault.path(), "alpha.md", "alpha content");

    let brain = make_brain(brain_dir.path()).await;
    let DocumentFreshness {
        reconciled_roots,
        skipped_roots,
        skipped_files,
        ..
    } = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    assert!(reconciled_roots.iter().any(|a| a == "vault"));
    assert!(
        skipped_roots.iter().any(|(alias, _)| alias == "missing"),
        "expected the missing root in skipped_roots: {skipped_roots:?}"
    );
    assert_eq!(skipped_files, 0);
}

#[tokio::test]
async fn search_response_keeps_planes_separate_when_both_requested() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "notes.md", "alpha content");

    let brain = make_brain(brain_dir.path()).await;
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    let q = oxibrain_core::retrieval::Query {
        text: "alpha".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [
            oxibrain_core::retrieval::SearchPlane::Memory,
            oxibrain_core::retrieval::SearchPlane::Documents,
        ]
        .into_iter()
        .collect(),
    };
    let SearchResponse {
        memory, documents, ..
    } = brain.search(q).await.unwrap();
    // Memory plane is empty (no episodes ingested) but must be present.
    assert!(memory.is_empty(), "memory should be empty");
    assert!(
        documents
            .iter()
            .any(|d| d.text.text.contains("alpha content")),
        "documents plane should still hit: {documents:?}"
    );
}

#[tokio::test]
async fn document_counts_reports_configured_roots_and_cached_files() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(vault.path(), "one.md", "one");
    write_file(vault.path(), "two.md", "two");

    let brain = make_brain(brain_dir.path()).await;
    // Fresh brain: cache absent ⇒ zero files, but the configured root counts.
    let (roots, files) = brain.document_counts().await.unwrap();
    assert_eq!((roots, files), (1, 0));

    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    let (roots, files) = brain.document_counts().await.unwrap();
    assert_eq!((roots, files), (1, 2));
}

#[tokio::test]
async fn dangling_document_refs_lists_only_unknown_aliases() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    let brain = make_brain(brain_dir.path()).await;
    let space = brain.ensure_space("personal").await.unwrap();

    for uri in ["doc://gone/note.md?rev=1", "doc://vault/note.md?rev=1"] {
        brain
            .ingest_event(
                &space,
                format!("body of {uri}"),
                oxibrain_core::SourceRef::Document { uri: uri.into() },
                oxibrain_core::TrustTier::Trusted,
                None,
                "test",
            )
            .await
            .unwrap();
    }

    let dangling = brain.dangling_document_refs().await.unwrap();
    assert_eq!(
        dangling,
        vec!["doc://gone/note.md?rev=1".to_string()],
        "only the alias missing from documents.toml is dangling"
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_registrations_all_persist() {
    // Regression: two concurrent register_document_root calls used to race
    // on the shared load-modify-save (one save could error or be lost).
    // The documents writer lock + unique temp files make every call land.
    let brain_dir = TempDir::new().unwrap();
    let brain = make_brain(brain_dir.path()).await;
    let vault_a = TempDir::new().unwrap();
    let vault_b = TempDir::new().unwrap();

    let (ra, rb) = tokio::join!(
        brain.register_document_root(oxibrain::document_plane::DocumentRootSpec {
            space: "personal".into(),
            alias: "vault-a".into(),
            path: vault_a.path().to_path_buf(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        }),
        brain.register_document_root(oxibrain::document_plane::DocumentRootSpec {
            space: "personal".into(),
            alias: "vault-b".into(),
            path: vault_b.path().to_path_buf(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        }),
    );
    ra.expect("registration A");
    rb.expect("registration B");

    let text = fs::read_to_string(brain_dir.path().join("documents.toml")).unwrap();
    assert!(text.contains("vault-a"), "{text}");
    assert!(text.contains("vault-b"), "{text}");
    assert_eq!(
        text.matches("[[root]]").count(),
        2,
        "both roots persisted exactly once each: {text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn registration_repairs_duplicate_alias_pollution() {
    // Flat-era configs can hold several [[root]] blocks sharing one
    // alias (and hundreds of dead test roots). Registration must repair
    // the duplicates instead of failing validation forever, and keep
    // the operator's original entry per alias.
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    fs::write(
        brain_dir.path().join("documents.toml"),
        format!(
            r#"[[root]]
alias = "vault"
path = "{}"
space = "personal"

[[root]]
alias = "vault"
path = "/tmp/gone-1"
space = "vault"

[[root]]
alias = "vault"
path = "/tmp/gone-2"
space = "vault"

[[root]]
alias = "knowledge"
path = "/tmp/also-gone"
space = "knowledge"
"#,
            vault.path().display()
        ),
    )
    .unwrap();
    let brain = make_brain(brain_dir.path()).await;
    let personal = TempDir::new().unwrap();
    let result = brain
        .register_document_root(oxibrain::document_plane::DocumentRootSpec {
            space: "personal".into(),
            alias: "personal".into(),
            path: personal.path().to_path_buf(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        })
        .await
        .expect("registration must repair the polluted config");
    assert_eq!(
        result.outcome,
        oxibrain::document_plane::RegisterRootOutcome::Added
    );
    let text = fs::read_to_string(brain_dir.path().join("documents.toml")).unwrap();
    assert_eq!(
        text.matches("alias = \"vault\"").count(),
        1,
        "duplicate vault aliases collapsed to the first: {text}"
    );
    assert!(
        !text.contains("/tmp/gone-1") && !text.contains("/tmp/gone-2"),
        "later duplicates dropped: {text}"
    );
    assert!(text.contains("alias = \"personal\""), "{text}");
    assert_eq!(
        text.matches("[[root]]").count(),
        3,
        "vault + knowledge + personal: {text}"
    );
}

// ── PDC classification, reporting, and projection (pdc-adoption-v1) ────────

const UUID_KNOWN: &str = "018f47c6-4a77-7c52-9db8-0e5f9bcb17db";

fn djot_source(id: &str, title: &str, extra_meta: &str, body: &str) -> String {
    format!(
        "---\nformat: pdc-document/1\nbody: pdc-djot/1\nid: {id}\n\
         created: 2026-09-13T12:34:56.789Z\nupdated: 2026-09-13T12:34:56.789Z\n\
         title: {title}\n{extra_meta}---\n{body}"
    )
}

#[tokio::test]
async fn pdc_and_legacy_classification_flows_into_freshness_report() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(
        vault.path(),
        "minimal.djot",
        &djot_source(
            UUID_KNOWN,
            "Minimal document",
            "",
            "# Minimal document\n\nThis is canonical Djot.\n",
        ),
    );
    // No envelope at all → invalid_transport, recorded, no upsert.
    write_file(vault.path(), "broken.djot", "plain words, no envelope\n");
    // Visible legacy HTML keeps the legacy adapter and counts as legacy_html.
    write_file(
        vault.path(),
        "notes.html",
        "<html><body><p>legacy prose lives here</p></body></html>\n",
    );
    // Markdown keeps the legacy decoder but is NOT a legacy HTML document;
    // the freshness count must not inflate.
    write_file(vault.path(), "notes.md", "plain markdown prose\n");

    let brain = make_brain(brain_dir.path()).await;
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    // Exactly one diagnostic: the broken djot. Nothing aborts the pass.
    assert_eq!(
        freshness.diagnostics.len(),
        1,
        "{:?}",
        freshness.diagnostics
    );
    let d = &freshness.diagnostics[0];
    assert_eq!(d.alias, "vault");
    assert_eq!(d.locator, "broken.djot");
    assert_eq!(d.code, "invalid_transport");
    assert_eq!(freshness.legacy_html, 1);

    // The canonical djot still indexed and its body text is searchable
    // (envelope never leaks into the cached text).
    let q = oxibrain_core::retrieval::Query {
        text: "canonical".into(),
        mode: oxibrain_core::retrieval::QueryMode::Lexical,
        space: "personal".into(),
        as_of: None,
        limit: 5,
        min_confidence: 0.0,
        planes: [oxibrain_core::retrieval::SearchPlane::Documents]
            .into_iter()
            .collect(),
    };
    let SearchResponse { documents, .. } = brain.search(q).await.unwrap();
    assert!(
        documents
            .iter()
            .any(|h| h.locator == "minimal.djot" && h.text.text.contains("canonical Djot")),
        "expected the djot hit: {documents:?}"
    );
}

#[tokio::test]
async fn duplicate_pdc_uuid_conflicts_are_reported_and_dropped() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(
        vault.path(),
        "a.djot",
        &djot_source(UUID_KNOWN, "First claim", "", "alpha words only here\n"),
    );
    write_file(
        vault.path(),
        "b.djot",
        &djot_source(UUID_KNOWN, "Second claim", "", "beta words only here\n"),
    );

    let brain = make_brain(brain_dir.path()).await;
    // The apply stage must not error even though two Add actions lose
    // their upserts — the facade converts them to Skip first.
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    let mut dup: Vec<_> = freshness
        .diagnostics
        .iter()
        .filter(|d| d.code == "duplicate_document_id")
        .collect();
    dup.sort_by(|a, b| a.locator.cmp(&b.locator));
    assert_eq!(dup.len(), 2, "{:?}", freshness.diagnostics);
    assert_eq!(dup[0].locator, "a.djot");
    assert!(dup[0].reason.contains("b.djot"), "{}", dup[0].reason);
    assert_eq!(dup[1].locator, "b.djot");
    assert!(dup[1].reason.contains("a.djot"), "{}", dup[1].reason);

    // Neither conflicting document landed in the cache.
    let cache = oxibrain_store::documents::DocumentCache::open_ro(brain_dir.path()).unwrap();
    assert!(cache.resolve_pdc("vault", UUID_KNOWN).unwrap().is_none());
}

#[tokio::test]
async fn unresolved_pdc_link_is_reported_per_document() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(
        vault.path(),
        "minimal.djot",
        &djot_source(UUID_KNOWN, "Minimal document", "", "target body words\n"),
    );
    let missing_uuid = "ffffffff-ffff-ffff-ffff-fffffffffff1";
    write_file(
        vault.path(),
        "linking.djot",
        &djot_source(
            "018f47c6-4a77-7c52-9db8-0e5f9bcb1700",
            "Linking document",
            "",
            &format!(
                "[Known](pdc://document/{UUID_KNOWN})\n\n[Missing](pdc://document/{missing_uuid})\n"
            ),
        ),
    );

    let brain = make_brain(brain_dir.path()).await;
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    let unresolved: Vec<_> = freshness
        .diagnostics
        .iter()
        .filter(|d| d.code == "unresolved_link")
        .collect();
    assert_eq!(unresolved.len(), 1, "{:?}", freshness.diagnostics);
    assert_eq!(unresolved[0].locator, "linking.djot");
    assert!(
        unresolved[0].reason.contains(missing_uuid),
        "{}",
        unresolved[0].reason
    );
    assert!(
        !unresolved[0].reason.contains(UUID_KNOWN),
        "resolved targets are not reported: {}",
        unresolved[0].reason
    );
}

#[tokio::test]
async fn deleted_pdc_document_lands_with_pdc_deleted_set() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    let uuid = "018f47c6-0000-7c52-9db8-0e5f9bcb17db";
    write_file(
        vault.path(),
        "trashed.djot",
        &djot_source(
            uuid,
            "Trashed note",
            "deleted: true\ndeleted_at: 2026-09-13T13:00:00.000Z\n",
            "archived contents\n",
        ),
    );

    let brain = make_brain(brain_dir.path()).await;
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    assert!(
        freshness.diagnostics.is_empty(),
        "{:?}",
        freshness.diagnostics
    );

    // Trash semantics: the row stays indexed with its canonical UUID and
    // the deletion state lands in the projection columns.
    let cache = oxibrain_store::documents::DocumentCache::open_ro(brain_dir.path()).unwrap();
    let (document_id, locator) = cache.resolve_pdc("vault", uuid).unwrap().unwrap();
    assert_eq!(locator, "trashed.djot");
    let conn = rusqlite::Connection::open_with_flags(
        brain_dir.path().join("documents.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (deleted, profile): (i64, String) = conn
        .query_row(
            "SELECT pdc_deleted, pdc_body_profile FROM documents WHERE id = ?1",
            rusqlite::params![document_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(profile, "pdc-djot/1");
}

#[tokio::test]
async fn pdc_uuid_link_resolution_survives_a_move() {
    let brain_dir = TempDir::new().unwrap();
    let vault = TempDir::new().unwrap();
    write_documents_config(brain_dir.path(), vault.path(), "personal");
    write_file(
        vault.path(),
        "original-name.djot",
        &djot_source(UUID_KNOWN, "Moved note", "", "stable identity body\n"),
    );

    let brain = make_brain(brain_dir.path()).await;
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();

    // Move (rename) the file: the cache key changes with the locator, but
    // the canonical PDC UUID must keep resolving — that is the Stage 2
    // exit criterion of doc/spec/pdc-adoption-v1.md.
    std::fs::create_dir(vault.path().join("renamed")).unwrap();
    fs::rename(
        vault.path().join("original-name.djot"),
        vault.path().join("renamed/deeper-name.djot"),
    )
    .unwrap();

    let brain = make_brain(brain_dir.path()).await;
    let freshness = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    assert!(
        freshness.diagnostics.is_empty(),
        "{:?}",
        freshness.diagnostics
    );

    let cache = oxibrain_store::documents::DocumentCache::open_ro(brain_dir.path()).unwrap();
    let (_, locator) = cache.resolve_pdc("vault", UUID_KNOWN).unwrap().unwrap();
    assert_eq!(locator, "renamed/deeper-name.djot");
}
