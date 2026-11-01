//! Routing, visibility and failure semantics; deterministic vectors are not model quality evidence.
use oxibrain::{
    Brain, BrainConfig, DocumentRootSpec, IndexOptions, ReadScope, RecallRequest, SearchRequest,
    Timestamp,
};
use oxibrain_core::context::render_context;
use oxibrain_core::retrieval::{QueryMode, SearchPlane};
use oxibrain_ports::{CharTokenizer, EmbeddingPort, FakeClock};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct Vectors {
    identity: &'static str,
    fail: AtomicBool,
    calls: AtomicUsize,
}
impl Vectors {
    fn new(identity: &'static str) -> Self {
        Self {
            identity,
            fail: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }
}
impl EmbeddingPort for Vectors {
    fn dim(&self) -> usize {
        1024
    }
    fn identity(&self) -> Option<String> {
        Some(self.identity.into())
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, oxibrain::BrainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(oxibrain::BrainError::Model(
                "fixture adapter failure".into(),
            ));
        }
        Ok(texts
            .iter()
            .map(|s| {
                let mut v = vec![0.0; 1024];
                v[usize::from(!(s.contains("CAR-LITERAL") || *s == "automobile"))] = 1.0;
                v
            })
            .collect())
    }
}
fn request(space: &str, query: &str, plane: SearchPlane) -> RecallRequest {
    let mut req = RecallRequest::new(space, query, 3000);
    req.planes = [plane].into_iter().collect();
    req
}

#[tokio::test]
async fn requested_space_reconciles_before_large_unrelated_root_without_losing_its_cache() {
    let dir = tempfile::tempdir().unwrap();
    let noise = tempfile::tempdir().unwrap();
    let wanted = tempfile::tempdir().unwrap();
    std::fs::write(noise.path().join("existing.md"), "cached unrelated source").unwrap();
    std::fs::write(wanted.path().join("existing.md"), "old requested source").unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_read_policy(oxibrain::ReadPolicy {
            reconcile_entries: 8,
            ..Default::default()
        })
        .unwrap();
    let space = brain.ensure_space("wanted-space").await.unwrap();
    // Registration order deliberately places the large unrelated root first.
    for (alias, root_space, path) in [
        ("noise", "noise-space", noise.path()),
        ("wanted", "wanted-space", wanted.path()),
    ] {
        brain
            .register_document_root(DocumentRootSpec {
                space: root_space.into(),
                alias: alias.into(),
                path: path.into(),
                include: None,
                exclude: None,
                max_file_bytes: None,
            })
            .await
            .unwrap();
    }
    brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    let cache = oxibrain_store::documents::DocumentCache::open_ro(dir.path()).unwrap();
    let prior_noise = cache.root_manifest("noise").unwrap();
    let prior_generation = cache.generation("noise").unwrap();
    drop(cache);
    for i in 0..32 {
        std::fs::write(
            noise.path().join(format!("noise-{i:03}.md")),
            "new unrelated source",
        )
        .unwrap();
    }
    let source = "fresh-requested-marker replacement procedure";
    std::fs::write(wanted.path().join("fresh.md"), source).unwrap();
    let result = brain
        .search_with_scope(SearchRequest {
            text: "fresh-requested-marker".into(),
            scope: ReadScope::current(&space),
            planes: [SearchPlane::Documents].into_iter().collect(),
            mode: QueryMode::Lexical,
            limit: 2,
        })
        .await
        .unwrap();
    assert!(
        result
            .meta
            .freshness
            .reconciled_roots
            .iter()
            .any(|alias| alias == "wanted")
    );
    assert!(
        result
            .meta
            .freshness
            .skipped_roots
            .iter()
            .any(|(alias, error)| alias == "noise" && error.contains("budget exceeded"))
    );
    let hit = result
        .data
        .documents
        .iter()
        .find(|hit| hit.locator == "fresh.md")
        .expect("requested root has fresh searchable bytes");
    assert_eq!(hit.text.text, source);
    assert_eq!(&source[hit.span_start..hit.span_end], hit.text.text);
    let cache = oxibrain_store::documents::DocumentCache::open_ro(dir.path()).unwrap();
    assert_eq!(cache.root_manifest("noise").unwrap(), prior_noise);
    assert_eq!(cache.generation("noise").unwrap(), prior_generation);
    assert_eq!(cache.list_roots().unwrap().len(), 2);
    drop(cache);

    // Explicit indexing remains global and is not limited by a read budget.
    let global = brain
        .index_documents(IndexOptions {
            embed: false,
            budget: None,
        })
        .await
        .unwrap();
    assert_eq!(global.reconciled_roots.len(), 2);
    let cache = oxibrain_store::documents::DocumentCache::open_ro(dir.path()).unwrap();
    assert_eq!(cache.root_manifest("noise").unwrap().len(), 33);
}

#[tokio::test]
async fn document_paraphrase_survives_unrelated_index_growth_and_dense_failure_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let source = "CAR-LITERAL replacement threshold 7300 km";
    std::fs::write(vault.path().join("car.md"), source).unwrap();
    let vectors = Arc::new(Vectors::new("fixture-document-family-v1"));
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_embedder(vectors.clone());
    let space = brain.ensure_space("fixture").await.unwrap();
    brain
        .register_document_root(DocumentRootSpec {
            space: "fixture".into(),
            alias: "vault".into(),
            path: vault.path().into(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        })
        .await
        .unwrap();
    let search = |mode| SearchRequest {
        text: "automobile".into(),
        scope: ReadScope::current(&space),
        planes: [SearchPlane::Documents].into_iter().collect(),
        mode,
        limit: 2,
    };
    brain
        .index_documents(IndexOptions {
            embed: true,
            budget: None,
        })
        .await
        .unwrap();
    let baseline = brain
        .search_with_scope(search(QueryMode::Hybrid))
        .await
        .unwrap();
    let original = baseline.data.documents.first().unwrap();
    assert_eq!(original.text.text, source);

    // These documents match the query literally but have unrelated fixture
    // vectors. Index growth must not discount the independent dense family.
    for i in 0..128 {
        std::fs::write(
            vault.path().join(format!("noise-{i:03}.md")),
            format!("automobile unrelated archive note {i}"),
        )
        .unwrap();
    }
    brain
        .index_documents(IndexOptions {
            embed: true,
            budget: None,
        })
        .await
        .unwrap();
    let grown = brain
        .search_with_scope(search(QueryMode::Hybrid))
        .await
        .unwrap();
    let retained = grown
        .data
        .documents
        .iter()
        .find(|hit| hit.locator == "car.md")
        .expect("semantic-only source survives the final two-candidate cap");
    assert_eq!(retained.text.text, source);
    assert_eq!(retained.revision, original.revision);
    assert_eq!(retained.view_digest, original.view_digest);
    assert_eq!(retained.decoder, original.decoder);
    assert_eq!(retained.span_start, 0);
    assert_eq!(retained.span_end, source.len());
    assert_eq!(
        retained.text.provenance.reference,
        original.text.provenance.reference
    );
    let dense = brain
        .search_with_scope(search(QueryMode::Dense))
        .await
        .unwrap();
    assert_eq!(dense.data.documents[0].locator, "car.md");

    let lexical = brain
        .search_with_scope(search(QueryMode::Lexical))
        .await
        .unwrap();
    vectors.fail.store(true, Ordering::SeqCst);
    let fallback = brain
        .search_with_scope(search(QueryMode::Hybrid))
        .await
        .unwrap();
    assert_eq!(
        fallback
            .data
            .documents
            .iter()
            .map(|hit| (&hit.locator, hit.score))
            .collect::<Vec<_>>(),
        lexical
            .data
            .documents
            .iter()
            .map(|hit| (&hit.locator, hit.score))
            .collect::<Vec<_>>()
    );
    assert!(fallback.meta.channels.iter().any(|channel| {
        channel.plane == "documents"
            && channel.channel == "dense"
            && channel
                .fallback
                .as_deref()
                .is_some_and(|error| error.contains("fixture adapter failure"))
    }));
}
#[tokio::test]
async fn indexed_documents_and_old_sessions_have_independent_dense_paths_and_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(
        vault.path().join("car.md"),
        "CAR-LITERAL service interval 7300 km",
    )
    .unwrap();
    let vectors = Arc::new(Vectors::new("fixture-car-v1"));
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_embedder(vectors.clone());
    let space = brain.ensure_space("fixture").await.unwrap();
    brain
        .register_document_root(DocumentRootSpec {
            space: "fixture".into(),
            alias: "vault".into(),
            path: vault.path().into(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        })
        .await
        .unwrap();
    brain
        .record_session(
            &space,
            "old-car",
            "CAR-LITERAL service interval 7300 km".into(),
            Timestamp(1),
        )
        .await
        .unwrap();
    for i in 2..12 {
        brain
            .record_session(
                &space,
                &format!("noise-{i}"),
                format!("unrelated observation {i}"),
                Timestamp(i),
            )
            .await
            .unwrap();
    }
    let unindexed = brain
        .recall_with_request(request(&space, "automobile", SearchPlane::Memory))
        .await
        .unwrap();
    assert!(!render_context(&unindexed.data.context).contains("7300"));
    let r = unindexed
        .meta
        .channels
        .iter()
        .find(|c| c.plane == "memory_sources" && c.channel == "dense")
        .unwrap();
    assert!(!r.executed && r.indexed == Some(0) && r.fallback.is_some());
    assert!(brain.embed_pending_memory(usize::MAX).await.unwrap() > 0);
    assert_eq!(brain.embed_pending_memory(usize::MAX).await.unwrap(), 0);
    brain
        .index_documents(IndexOptions {
            embed: true,
            budget: None,
        })
        .await
        .unwrap();
    for plane in [SearchPlane::Memory, SearchPlane::Documents] {
        let result = brain
            .recall_with_request(request(&space, "automobile", plane))
            .await
            .unwrap();
        assert!(render_context(&result.data.context).contains("7300"));
        let receipt = result
            .meta
            .channels
            .iter()
            .find(|c| {
                c.channel == "dense"
                    && c.plane
                        == if plane == SearchPlane::Memory {
                            "memory_sources"
                        } else {
                            "documents"
                        }
            })
            .unwrap();
        assert!(receipt.executed && receipt.fallback.is_none());
        assert_eq!(receipt.indexed, receipt.total);
        assert!(
            result
                .data
                .evidence
                .iter()
                .any(|c| c.role == "untrusted_observation")
        );
    }
    // Ordinary reads embed query text, never rebuild missing source vectors.
    vectors.calls.store(0, Ordering::SeqCst);
    brain
        .recall_with_request(request(&space, "automobile", SearchPlane::Memory))
        .await
        .unwrap();
    assert_eq!(vectors.calls.load(Ordering::SeqCst), 1);
    vectors.fail.store(true, Ordering::SeqCst);
    let fallback = brain
        .recall_with_request(request(&space, "7300", SearchPlane::Memory))
        .await
        .unwrap();
    assert!(render_context(&fallback.data.context).contains("7300"));
    assert!(fallback.meta.channels.iter().any(|c| {
        c.fallback
            .as_deref()
            .is_some_and(|s| s.contains("fixture adapter failure"))
    }));
    let changed = Brain::open(BrainConfig::at(dir.path()))
        .await
        .unwrap()
        .with_embedder(Arc::new(Vectors::new("fixture-car-v2")));
    let result = changed
        .recall_with_request(request(&space, "automobile", SearchPlane::Memory))
        .await
        .unwrap();
    assert!(!render_context(&result.data.context).contains("7300"));
    assert!(
        result
            .meta
            .channels
            .iter()
            .any(|c| c.plane == "memory_sources" && c.indexed == Some(0))
    );
}

#[tokio::test]
async fn raw_source_query_terms_deduplicate_and_channel_caps_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(100)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path()), clock)
        .await
        .unwrap()
        .with_tokenizer(Arc::new(CharTokenizer))
        .with_read_policy(oxibrain::ReadPolicy {
            channel_depth: 1,
            query_terms: 2,
            semantic_sources: false,
            ..Default::default()
        })
        .unwrap();
    let space = brain.ensure_space("fixture").await.unwrap();
    brain
        .record_session(
            &space,
            "needle-source",
            "Needle marker from first source".into(),
            Timestamp(10),
        )
        .await
        .unwrap();
    brain
        .record_session(
            &space,
            "tail-source",
            "Tail marker from second source".into(),
            Timestamp(20),
        )
        .await
        .unwrap();

    let query = format!("{}tail", "Needle needle ".repeat(32));
    let response = brain
        .recall_with_request(request(&space, &query, SearchPlane::Memory))
        .await
        .unwrap();
    let rendered = render_context(&response.data.context);
    assert!(rendered.contains("Needle marker from first source"));
    assert!(rendered.contains("Tail marker from second source"));

    for name in ["word", "ngram"] {
        let receipt = response
            .meta
            .channels
            .iter()
            .find(|channel| channel.plane == "memory_sources" && channel.channel == name)
            .unwrap();
        assert_eq!(
            receipt.algorithm_version.as_deref(),
            Some("raw-source-terms/1")
        );
        assert_eq!(receipt.query_limit, Some(2));
        assert_eq!(receipt.plan_candidates_examined, 2);
        assert_eq!(receipt.plan_candidates_selected, 2);
        assert_eq!(receipt.queries_run, 2);
        assert_eq!(receipt.considered, 2);
        assert!(receipt.limit_reached);
        assert!(!receipt.capped);
        assert!(receipt.partial());
    }
    assert_eq!(response.meta.execution["partial"], true);

    let capped = brain
        .clone()
        .with_read_policy(oxibrain::ReadPolicy {
            channel_depth: 64,
            query_terms: 1,
            semantic_sources: false,
            ..Default::default()
        })
        .unwrap()
        .recall_with_request(request(&space, "Needle tail", SearchPlane::Memory))
        .await
        .unwrap();
    let receipt = capped
        .meta
        .channels
        .iter()
        .find(|channel| channel.plane == "memory_sources" && channel.channel == "word")
        .unwrap();
    assert_eq!(receipt.plan_candidates_examined, 2);
    assert_eq!(receipt.plan_candidates_selected, 1);
    assert_eq!(receipt.queries_run, 1);
    assert!(receipt.capped);
    assert!(!receipt.limit_reached);
    assert_eq!(capped.meta.execution["partial"], true);
}

#[tokio::test]
async fn dense_source_scope_time_session_exclusion_and_scan_caps_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(100)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path()), clock.clone())
        .await
        .unwrap()
        .with_embedder(Arc::new(Vectors::new("fixture-scope-v1")));
    let space = brain.ensure_space("inside").await.unwrap();
    let other = brain.ensure_space("outside").await.unwrap();
    brain
        .record_session(
            &space,
            "eligible",
            "CAR-LITERAL inside-marker".into(),
            Timestamp(10),
        )
        .await
        .unwrap();
    brain
        .record_session(
            &other,
            "outside",
            "CAR-LITERAL outside-marker".into(),
            Timestamp(10),
        )
        .await
        .unwrap();
    clock.advance(100);
    brain
        .record_session(
            &space,
            "future",
            "CAR-LITERAL future-marker".into(),
            Timestamp(150),
        )
        .await
        .unwrap();
    brain.embed_pending_memory(usize::MAX).await.unwrap();
    let mut req = request(&space, "automobile", SearchPlane::Memory);
    req.scope = ReadScope {
        space: space.clone(),
        as_of: Some(Timestamp(50)),
        known_at: Some(Timestamp(100)),
        min_confidence: 0.0,
    };
    let response = brain.recall_with_request(req.clone()).await.unwrap();
    let text = render_context(&response.data.context);
    assert!(text.contains("inside-marker"));
    assert!(!text.contains("outside-marker") && !text.contains("future-marker"));
    req.session_id = Some("eligible".into());
    let excluded = brain.recall_with_request(req).await.unwrap();
    assert!(!render_context(&excluded.data.context).contains("inside-marker"));
    let bounded = brain
        .clone()
        .with_read_policy(oxibrain::ReadPolicy {
            dense_scan_vectors: 1,
            ..Default::default()
        })
        .unwrap();
    let response = bounded
        .recall_with_request(request(&space, "automobile", SearchPlane::Memory))
        .await
        .unwrap();
    let receipt = response
        .meta
        .channels
        .iter()
        .find(|c| c.plane == "memory_sources" && c.channel == "dense")
        .unwrap();
    assert!(receipt.capped);
    assert_eq!(receipt.considered, 1);
    assert_eq!(response.meta.execution["partial"], true);
    brain.reproject().await.unwrap();
    let rebuilt = brain
        .recall_with_request(request(&space, "automobile", SearchPlane::Memory))
        .await
        .unwrap();
    let receipt = rebuilt
        .meta
        .channels
        .iter()
        .find(|c| c.plane == "memory_sources" && c.channel == "dense")
        .unwrap();
    assert_eq!(receipt.indexed, receipt.total);
}
