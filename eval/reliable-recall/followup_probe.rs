//! Research diagnostics for 40482ee (0.20.1), not acceptance tests for desired behavior.
//! Copy to crates/oxibrain-cli/tests/research_followup_probe.rs to run; see the report.
//! All writes are synthetic and isolated. No model or provider is used.
use oxibrain::{
    Brain, BrainConfig, DeclObject, Declaration, EntityRef, IndexOptions, ReadPolicy, ReadScope,
    RecallRequest, SearchRequest, Timestamp,
};
use oxibrain_core::context::{LayerKind, render_context};
use oxibrain_core::evidence::{EvidenceUnit, SourceCoordinate};
use oxibrain_core::retrieval::{QueryMode, SearchPlane};
use oxibrain_ports::{EmbeddingPort, TokenizerPort};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn emit(name: &str, value: Value) {
    let dir = std::path::PathBuf::from(std::env::var("OXIBRAIN_RESEARCH_OUTPUT").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
    println!("FOLLOWUP {name} {value}");
}

fn tokenizer() -> Arc<dyn TokenizerPort> {
    Arc::new(
        oxibrain_tokenizer_local::FileTokenizer::open(std::path::Path::new(
            &std::env::var("OXIBRAIN_TOKENIZER_JSON").unwrap(),
        ))
        .unwrap(),
    )
}

fn note(body: &str, ordinal: usize) -> String {
    format!(
        "---\nformat: pdc-document/2\nbody: pdc-markdown/1\nid: 018f47c6-4a77-7c52-9db8-{ordinal:012}\ncreated: 2026-09-14T12:34:56.789Z\nupdated: 2026-09-14T12:34:56.789Z\ntitle: Fixture\n---\n{body}\n"
    )
}

fn roots(store: &std::path::Path, entries: &[(&str, &std::path::Path, &str)]) {
    let config: String = entries
        .iter()
        .map(|(alias, path, space)| {
            format!(
                "[[root]]\nalias={alias:?}\npath={}\nspace={space:?}\n",
                serde_json::to_string(&path.to_string_lossy()).unwrap()
            )
        })
        .collect();
    std::fs::write(store.join("documents.toml"), config).unwrap();
}

fn search(
    space: &str,
    query: &str,
    plane: SearchPlane,
    mode: QueryMode,
    limit: usize,
) -> SearchRequest {
    SearchRequest {
        text: query.into(),
        scope: ReadScope::current(space),
        planes: [plane].into_iter().collect(),
        mode,
        limit,
    }
}

fn recall(space: &str, query: &str, plane: SearchPlane) -> RecallRequest {
    let mut r = RecallRequest::new(space, query, 2000);
    r.planes = [plane].into_iter().collect();
    r.require_exact_tokens = true;
    r
}

#[derive(Default)]
struct DeterministicVectors {
    calls: AtomicUsize,
}
impl EmbeddingPort for DeterministicVectors {
    fn dim(&self) -> usize {
        1024
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, oxibrain::BrainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0; 1024];
                if t.contains("INSIDE") {
                    v[0] = 0.8;
                    v[1] = 0.6;
                } else {
                    v[0] = 1.0;
                }
                v
            })
            .collect())
    }
}

#[tokio::test]
async fn heading_hit_does_not_imply_body_support() {
    let store = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let bytes = note(
        "# OrionRecovery\n\nApproved rollback command: restore-release-53.\n\nRun only after the health check has failed.",
        1,
    );
    let path = vault.path().join("policy.md");
    std::fs::write(&path, &bytes).unwrap();
    roots(store.path(), &[("vault", vault.path(), "fixture")]);
    let tok = tokenizer();
    let b = Brain::open(BrainConfig::at(store.path()))
        .await
        .unwrap()
        .with_tokenizer(tok.clone());
    let space = b.ensure_space("fixture").await.unwrap();
    let hits = b
        .search_with_scope(search(
            &space,
            "OrionRecovery",
            SearchPlane::Documents,
            QueryMode::Lexical,
            20,
        ))
        .await
        .unwrap();
    let r = b
        .recall_with_request(recall(&space, "OrionRecovery", SearchPlane::Documents))
        .await
        .unwrap();
    let text = render_context(&r.data.context);
    let payload = hits
        .data
        .documents
        .iter()
        .map(|h| h.text.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let control = b
        .recall_with_request(recall(&space, "restore-release-53", SearchPlane::Documents))
        .await
        .unwrap();
    assert!(render_context(&control.data.context).contains("restore-release-53"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes.as_bytes());
    emit(
        "heading",
        json!({"search_hits":hits.data.documents.len(),
        "title_query_has_answer":text.contains("restore-release-53"),
        "answer_query_has_answer":true,"payload":payload,
        "context":text,"context_tokens":r.data.context.total_tokens,
        "payload_tokens":tok.count(&payload),"budget":2000,"source_unchanged":true,
        "tokenizer_digest":tok.artifact_digest(),"evidence":r.data.evidence}),
    );
}

#[tokio::test]
async fn configured_vectors_do_not_reach_all_recall_planes() {
    let store = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(
        vault.path().join("vehicle.md"),
        note("Vehicle service interval is 7300 km for the car.", 2),
    )
    .unwrap();
    roots(store.path(), &[("vault", vault.path(), "fixture")]);
    let vectors = Arc::new(DeterministicVectors::default());
    let b = Brain::open(BrainConfig::at(store.path()))
        .await
        .unwrap()
        .with_tokenizer(tokenizer())
        .with_embedder(vectors.clone());
    let space = b.ensure_space("fixture").await.unwrap();
    b.index_documents(IndexOptions {
        embed: true,
        budget: None,
    })
    .await
    .unwrap();
    let docs = b
        .search_with_scope(search(
            &space,
            "automobile",
            SearchPlane::Documents,
            QueryMode::Hybrid,
            10,
        ))
        .await
        .unwrap();
    assert!(
        docs.data
            .documents
            .iter()
            .any(|h| h.text.text.contains("7300"))
    );
    let doc_recall = b
        .recall_with_request(recall(&space, "automobile", SearchPlane::Documents))
        .await
        .unwrap();
    b.record_session(
        &space,
        "old-vehicle",
        "Vehicle service interval is 7300 km for the car.".into(),
        Timestamp(1),
    )
    .await
    .unwrap();
    for i in 2..12 {
        b.record_session(
            &space,
            &format!("new-{i}"),
            format!("Unrelated record {i}"),
            Timestamp(i),
        )
        .await
        .unwrap();
    }
    vectors.calls.store(0, Ordering::SeqCst);
    let raw = b
        .recall_with_request(recall(&space, "automobile", SearchPlane::Memory))
        .await
        .unwrap();
    let calls = vectors.calls.load(Ordering::SeqCst);
    let literal = b
        .recall_with_request(recall(&space, "7300", SearchPlane::Memory))
        .await
        .unwrap();
    assert!(render_context(&literal.data.context).contains("7300"));
    emit(
        "dense_routing",
        json!({"embedder":"deterministic test vector, not a semantic model",
        "hybrid_document_search_has_source":true,
        "document_recall_has_source":render_context(&doc_recall.data.context).contains("7300"),
        "raw_paraphrase_has_source":render_context(&raw.data.context).contains("7300"),
        "raw_literal_has_source":true,"raw_paraphrase_embedding_calls":calls,
        "execution":raw.meta.execution,"document_execution":doc_recall.meta.execution}),
    );
}

#[tokio::test]
async fn document_dense_limit_precedes_space_filter() {
    let store = tempfile::tempdir().unwrap();
    let inside = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        inside.path().join("inside.md"),
        note("INSIDE relevant fact.", 3),
    )
    .unwrap();
    std::fs::write(
        outside.path().join("outside.md"),
        note("OUTSIDE unrelated fact.", 4),
    )
    .unwrap();
    roots(
        store.path(),
        &[
            ("inside", inside.path(), "requested"),
            ("outside", outside.path(), "other"),
        ],
    );
    let b = Brain::open(BrainConfig::at(store.path()))
        .await
        .unwrap()
        .with_tokenizer(tokenizer())
        .with_embedder(Arc::new(DeterministicVectors::default()));
    let space = b.ensure_space("requested").await.unwrap();
    b.ensure_space("other").await.unwrap();
    b.index_documents(IndexOptions {
        embed: true,
        budget: None,
    })
    .await
    .unwrap();
    let small = b
        .search_with_scope(search(
            &space,
            "zzqqsearch",
            SearchPlane::Documents,
            QueryMode::Dense,
            1,
        ))
        .await
        .unwrap();
    let larger = b
        .search_with_scope(search(
            &space,
            "zzqqsearch",
            SearchPlane::Documents,
            QueryMode::Dense,
            2,
        ))
        .await
        .unwrap();
    assert_eq!(larger.data.documents.len(), 1);
    assert!(larger.data.documents[0].text.text.contains("INSIDE"));
    emit(
        "space_cap",
        json!({"requested_space_documents":1,"other_space_documents":1,
        "limit_1_hits":small.data.documents.len(),"limit_2_hits":larger.data.documents.len(),
        "outside_content_returned":small.data.documents.iter().chain(larger.data.documents.iter()).any(|h|h.text.text.contains("OUTSIDE")),
        "limit_1_execution":small.meta.execution,
        "embedder":"deterministic routing fixture, not model quality"}),
    );
}

#[tokio::test]
async fn complete_truth_groups_are_capped_before_query_selection() {
    let store = tempfile::tempdir().unwrap();
    let mut b = Brain::open(BrainConfig::at(store.path()))
        .await
        .unwrap()
        .with_tokenizer(tokenizer());
    let space = b.ensure_space("fixture").await.unwrap();
    for i in 0..24 {
        b.declare(
            &space,
            Declaration::AddStatement {
                subject: EntityRef {
                    surface: format!("Person{i:03}"),
                    ty: "Person".into(),
                },
                predicate: "full_name".into(),
                object: DeclObject::Literal {
                    literal_type: "text".into(),
                    value: format!("LegalName{i:03}"),
                },
                polarity: "affirm".into(),
                valid_from: 0,
                valid_to: i64::MAX,
            },
        )
        .await
        .unwrap();
    }
    let all = b
        .search_with_scope(search(
            &space,
            "LegalName",
            SearchPlane::Memory,
            QueryMode::Lexical,
            100,
        ))
        .await
        .unwrap();
    assert_eq!(all.data.memory.len(), 24);
    b = b
        .with_read_policy(ReadPolicy {
            truth_bytes: 512,
            ..ReadPolicy::default()
        })
        .unwrap();
    let limited = b
        .search_with_scope(search(
            &space,
            "LegalName",
            SearchPlane::Memory,
            QueryMode::Lexical,
            100,
        ))
        .await
        .unwrap();
    let omitted = all
        .data
        .memory
        .iter()
        .find(|h| {
            !limited
                .data
                .memory
                .iter()
                .any(|l| l.entity_id == h.entity_id)
        })
        .unwrap();
    let targeted = b
        .search_with_scope(search(
            &space,
            &omitted.entity_surface,
            SearchPlane::Memory,
            QueryMode::Lexical,
            100,
        ))
        .await
        .unwrap();
    emit(
        "truth_cap",
        json!({"default_truth_bytes":8388608,"probe_truth_bytes":512,
        "uncapped_hits":all.data.memory.len(),"capped_hits":limited.data.memory.len(),
        "explicit_target":omitted.entity_surface,"target_returned":targeted.data.memory.iter().any(|h|h.entity_id==omitted.entity_id),
        "dropped":targeted.meta.dropped,"execution":targeted.meta.execution,
        "qualification":"reduced-cap mechanism test; no default-size production outage demonstrated"}),
    );
}

struct CountedTokenizer {
    inner: Arc<dyn TokenizerPort>,
    calls: AtomicUsize,
    bytes: AtomicUsize,
}
impl TokenizerPort for CountedTokenizer {
    fn count(&self, s: &str) -> usize {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.bytes.fetch_add(s.len(), Ordering::SeqCst);
        self.inner.count(s)
    }
    fn truncate_to(&self, s: &str, n: usize) -> String {
        self.inner.truncate_to(s, n)
    }
    fn id(&self) -> &str {
        self.inner.id()
    }
}

#[test]
fn packer_tokenizer_work_and_short_source_cost() {
    let tok = tokenizer();
    let mut rows = Vec::new();
    for n in [8, 32, 64] {
        let units: Vec<_> = (0..n)
            .map(|i| {
                EvidenceUnit::new(
                    LayerKind::SessionSummaries,
                    format!("Approval {i}: rollback only after health failure."),
                    SourceCoordinate {
                        reference: format!("session://fixture-{i}"),
                        revision: "r1".into(),
                        view: "episode-utf8/1".into(),
                        view_digest: Some("r1".into()),
                        decoder: None,
                        start: 0,
                        end: format!("Approval {i}: rollback only after health failure.").len(),
                        document_uuid: None,
                    },
                    "untrusted_observation",
                )
            })
            .collect();
        let counted = CountedTokenizer {
            inner: tok.clone(),
            calls: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        };
        let start = std::time::Instant::now();
        let r = oxibrain_core::evidence_pack::pack_evidence(&units, 2000, &counted);
        let millis = start.elapsed().as_secs_f64() * 1000.0;
        let context = render_context(&r.context);
        assert!(tok.count(&context) <= 2000);
        let payload = units
            .iter()
            .take(r.evidence.len())
            .map(|u| u.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        rows.push(json!({"candidates":n,"selected":r.evidence.len(),"context_tokens":r.context.total_tokens,
            "payload_tokens":tok.count(&payload),"count_calls":counted.calls.load(Ordering::SeqCst),
            "counted_input_bytes":counted.bytes.load(Ordering::SeqCst),"context_bytes":context.len(),"elapsed_ms":millis}));
    }
    emit(
        "packing",
        json!({"rows":rows,"budget":2000,"tokenizer_digest":tok.artifact_digest(),
        "qualification":"one debug-build run; operation counts are deterministic, latency is not an isolated benchmark; standalone payload counts are not an additive token allocation"}),
    );
}
