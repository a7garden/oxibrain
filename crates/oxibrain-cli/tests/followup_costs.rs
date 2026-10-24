//! Explicit real-tokenizer measurements, separate from default CI.
#[path = "../../../eval/reliable-recall/pack_reference.rs"]
mod reference;
use oxibrain_core::{
    context::LayerKind,
    evidence::{EvidenceUnit, SourceCoordinate},
};
use oxibrain_ports::TokenizerPort;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Counted<'a> {
    inner: &'a dyn TokenizerPort,
    calls: AtomicUsize,
    bytes: AtomicUsize,
}
impl TokenizerPort for Counted<'_> {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn count(&self, text: &str) -> usize {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(text.len(), Ordering::Relaxed);
        self.inner.count(text)
    }
    fn truncate_to(&self, text: &str, n: usize) -> String {
        self.inner.truncate_to(text, n)
    }
}
#[test]
#[ignore = "explicit real-tokenizer release measurement"]
fn measure_canonical_packing_cost_and_byte_equivalence() {
    let tokenizer = oxibrain_tokenizer_local::FileTokenizer::open(std::path::Path::new(
        &std::env::var("OXIBRAIN_TOKENIZER_JSON").unwrap(),
    ))
    .unwrap();
    let mut observations = Vec::new();
    for n in [8, 32, 64] {
        let units: Vec<_> = (0..n)
            .map(|i| {
                let payload =
                    format!("Payload {i}: restore the approved checkpoint only after checks fail.");
                let end = payload.len();
                EvidenceUnit::new(
                    LayerKind::Documents,
                    payload,
                    SourceCoordinate {
                        reference: format!("doc://fixture/note-{i}?rev=stable"),
                        revision: "stable".into(),
                        view: "decoded-utf8/1".into(),
                        view_digest: Some("digest".into()),
                        decoder: Some("3:pdc-markdown/1".into()),
                        start: 0,
                        end,
                        document_uuid: None,
                    },
                    "untrusted_observation",
                )
            })
            .collect();
        let mut results = Vec::new();
        let mut runs = Vec::new();
        for optimized in [false, true] {
            let counter = Counted {
                inner: &tokenizer,
                calls: AtomicUsize::new(0),
                bytes: AtomicUsize::new(0),
            };
            let started = std::time::Instant::now();
            let result = if optimized {
                oxibrain_core::evidence_pack::pack_evidence(&units, 2000, &counter)
            } else {
                reference::pack_evidence(&units, 2000, &counter)
            };
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let serialization_started = std::time::Instant::now();
            let serialized = serde_json::to_vec(&result).unwrap();
            let final_serialization_ms = serialization_started.elapsed().as_secs_f64() * 1000.0;
            runs.push(serde_json::json!({"final_serialization_ms":final_serialization_ms,"serialized_bytes":serialized.len(),"optimized":optimized,"calls":counter.calls.load(Ordering::Relaxed),"bytes":counter.bytes.load(Ordering::Relaxed),"elapsed_ms":elapsed_ms,"selected":result.evidence.len(),"tokens":result.context.total_tokens}));
            results.push(serde_json::to_value(result).unwrap());
        }
        assert_eq!(results[0], results[1]);
        observations.push(serde_json::json!({"candidates":n,"runs":runs,"byte_equivalent":true}));
    }
    std::fs::write(
        std::env::var("OXIBRAIN_PACK_OUTPUT").unwrap(),
        serde_json::to_vec_pretty(&observations).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "explicit local multilingual model cost measurement"]
async fn measure_cold_model_and_warm_source_query() {
    use std::sync::Arc;
    let model = std::env::var("OXIBRAIN_EMBED_MODEL").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path()))
        .await
        .unwrap();
    let space = brain.ensure_space("cost").await.unwrap();
    brain
        .record_session(
            &space,
            "old-car",
            "The car needs servicing every 7300 kilometers.".into(),
            oxibrain::Timestamp(1),
        )
        .await
        .unwrap();
    for i in 0..10 {
        brain
            .record_session(
                &space,
                &format!("noise-{i}"),
                format!("Other observation {i}"),
                oxibrain::Timestamp(2 + i),
            )
            .await
            .unwrap();
    }
    let brain = brain.with_embedder(Arc::new(
        oxibrain_embed_local::LocalEmbedder::open(std::path::Path::new(&model), Default::default())
            .unwrap(),
    ));
    brain.embed_pending_memory(usize::MAX).await.unwrap();
    // Independent fixed texts test batch invariance, not ranking acceptance.
    let embedder = brain.embedder().unwrap();
    let texts = [
        "automobile maintenance interval",
        "The car needs servicing every 7300 kilometers.",
        "Die Ausstellung öffnet jeden Morgen um 09:45 Uhr.",
    ];
    let batch = embedder.embed(&texts).unwrap();
    let single = embedder.embed(&[texts[1]]).unwrap();
    let max_batch_difference = single[0]
        .iter()
        .zip(&batch[1])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_batch_difference < 0.001,
        "embedding depends on preceding batch inputs"
    );
    let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
    let probe = serde_json::json!({"texts":texts,"batch_max_absolute_difference":max_batch_difference,"query_car_cosine":cosine(&batch[0],&batch[1]),"query_exhibition_cosine":cosine(&batch[0],&batch[2]),"scope":"Batch-invariance sanity check, not a ranking benchmark."});
    drop(brain);
    let mut observations = Vec::new();
    for run in 0..2 {
        let start = std::time::Instant::now();
        let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path()))
            .await
            .unwrap()
            .with_embedder(Arc::new(
                oxibrain_embed_local::LocalEmbedder::open(
                    std::path::Path::new(&model),
                    Default::default(),
                )
                .unwrap(),
            ));
        let load_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut times = Vec::new();
        let mut hit = false;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let mut req =
                oxibrain::RecallRequest::new(&space, "automobile maintenance interval", 2000);
            req.planes = [oxibrain_core::retrieval::SearchPlane::Memory]
                .into_iter()
                .collect();
            let result = brain.recall_with_request(req).await.unwrap();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
            hit = oxibrain_core::context::render_context(&result.data.context).contains("7300");
        }
        observations.push(serde_json::json!({"run":run,"model_load_ms":load_ms,"cold_model_plus_first_query_ms":load_ms+times[0],"warm_query_ms":times,"source_retrieved":hit,"scope":"Cold model in process, warm OS/index; not a process-cold p95 claim."}));
    }
    std::fs::write(
        std::env::var("OXIBRAIN_QUERY_COST_OUTPUT").unwrap(),
        serde_json::to_vec_pretty(
            &serde_json::json!({"embedding_probe":probe,"query_runs":observations}),
        )
        .unwrap(),
    )
    .unwrap();
}
