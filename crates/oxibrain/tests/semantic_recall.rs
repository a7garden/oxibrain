//! Routing, visibility and failure semantics; deterministic vectors are not model quality evidence.
use oxibrain::{
    Brain, BrainConfig, DocumentRootSpec, IndexOptions, ReadScope, RecallRequest, Timestamp,
};
use oxibrain_core::context::render_context;
use oxibrain_core::retrieval::SearchPlane;
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
                v[usize::from(!(s.contains("CAR-LITERAL") || s.contains("automobile")))] = 1.0;
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
