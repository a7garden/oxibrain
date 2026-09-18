//! §9.5 extractor-identity integration tests: the ExtractorConfig bound on
//! the Brain (model id, mechanism, weights digest) must key the extraction
//! cache. Same identity re-hits; a weight change must re-extract instead of
//! silently reusing extractions produced by the old weights (the model-swap
//! bug: the backlog drain kept a static facade identity across swaps).

use oxibrain::{Brain, BrainConfig, CaptureOutcome};
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_ports::{FakeClock, FakeLlmPort, LlmResponse, Timestamp};
use rusqlite::Connection;
use std::sync::Arc;
use tempfile::TempDir;

fn canned_response() -> &'static str {
    r#"{"claims":[
        {"predicate":"works_on","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.95},
        {"predicate":"employed_by","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"Acme Corp","entity_type":"Organization","span":[27,36]}},"polarity":"affirm","confidence":0.9}
    ]}"#
}

fn extractor(model: &str, digest: &str) -> ExtractorConfig {
    ExtractorConfig {
        model_id: model.into(),
        prompt_version: 2,
        registry_major: 1,
        mechanism: ExtractMechanism::JsonSchema,
        max_tokens: 4096,
        model_digest: Some(digest.into()),
        provider_profile_id: None,
    }
}

fn content() -> String {
    "Alice works on ProjectX at Acme Corp".into()
}

async fn brain_with(dir: &TempDir, llm: Arc<FakeLlmPort>, cfg: ExtractorConfig) -> Brain {
    Brain::with_llm(
        BrainConfig::at(dir.path().to_str().unwrap()),
        Arc::new(FakeClock::new(Timestamp::from_millis(10000))),
        llm,
    )
    .await
    .unwrap()
    .with_extractor_config(cfg)
}

fn extraction_ids(dir: &TempDir) -> Vec<String> {
    let conn = Connection::open_with_flags(
        dir.path().join("brain.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stmt = conn
        .prepare("SELECT DISTINCT extractor_id FROM extractions ORDER BY extractor_id")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[tokio::test]
async fn remember_extracts_under_bound_identity() {
    let dir = TempDir::new().unwrap();
    let cfg = extractor("m-a", "digest-a");

    let llm = Arc::new(FakeLlmPort::new());
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_response().into(),
            raw: serde_json::Value::Null,
        },
    );
    let brain = brain_with(&dir, llm, cfg.clone()).await;

    let outcome = brain
        .remember("test", "test.md".into(), content(), Timestamp::from_millis(20000))
        .await
        .unwrap();
    assert!(
        matches!(outcome, CaptureOutcome::Captured { .. }),
        "inline extraction must run under the bound identity: {outcome:?}"
    );

    // The cache row must carry the BOUND extractor id, not the static
    // facade default ("oxibrain-default").
    assert_eq!(extraction_ids(&dir), vec![cfg.id()]);
}

#[tokio::test]
async fn same_identity_cache_hits_weight_change_re_extracts() {
    let dir = TempDir::new().unwrap();
    let cfg_a = extractor("m-a", "digest-a");

    // Round 1 — capture + inline extraction under identity A.
    let llm1 = Arc::new(FakeLlmPort::new());
    llm1.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_response().into(),
            raw: serde_json::Value::Null,
        },
    );
    let brain1 = brain_with(&dir, llm1, cfg_a.clone()).await;
    brain1
        .remember("test", "test.md".into(), content(), Timestamp::from_millis(20000))
        .await
        .unwrap();

    // Round 2 — same identity, fresh Brain: pure cache hit, nothing drains.
    let brain2 = brain_with(&dir, Arc::new(FakeLlmPort::new()), cfg_a.clone()).await;
    assert_eq!(brain2.extract_uncached(10).await.unwrap(), 0);

    // Round 3 — same model, different weights digest: must re-extract.
    let llm3 = Arc::new(FakeLlmPort::new());
    llm3.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_response().into(),
            raw: serde_json::Value::Null,
        },
    );
    let brain3 = brain_with(&dir, llm3, extractor("m-a", "digest-b")).await;
    assert_eq!(brain3.extract_uncached(10).await.unwrap(), 1);
}
