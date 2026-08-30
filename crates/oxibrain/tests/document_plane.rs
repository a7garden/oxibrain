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
