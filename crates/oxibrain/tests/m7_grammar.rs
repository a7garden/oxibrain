//! Grammar-constrained extraction path (§9.4, D28): when the LLM advertises
//! GBNF support, the pipeline must call `generate_constrained` with a grammar
//! generated from the predicate registry — not `complete` with a JSON Schema.

use oxibrain::Brain;
use oxibrain_core::SourceRef;
use oxibrain_core::TrustTier;
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_ports::{FakeClock, FakeLlmPort, LlmResponse, Timestamp};

fn canned_response() -> &'static str {
    r#"{"claims":[
        {"predicate":"works_on","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.95}
    ]}"#
}

#[tokio::test]
async fn grammar_capable_llm_takes_the_constrained_branch() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path());

    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm = std::sync::Arc::new(FakeLlmPort::new());
    llm.enable_grammar();
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_response().into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain = Brain::with_llm(config, clock, llm.clone()).await.unwrap();
    let space = brain.ensure_space("test").await.unwrap();
    let ep_id = brain
        .ingest(
            &space,
            "Alice works on ProjectX".into(),
            SourceRef::Note {
                path: "n.md".into(),
            },
            TrustTier::Trusted,
            "fake-extractor",
        )
        .await
        .unwrap();

    let extractor = ExtractorConfig {
        model_id: "fake-grammar-model".into(),
        prompt_version: 1,
        registry_major: 1,
        mechanism: ExtractMechanism::Grammar,
        max_tokens: 4096,
        model_digest: None,
        provider_profile_id: None,
    };
    let summary = brain.extract_one(&space, &ep_id, &extractor).await.unwrap();

    assert_eq!(summary.extracted, 1, "claim must project");
    assert!(
        llm.constrained_calls() >= 1,
        "grammar-capable LLM must be driven via generate_constrained"
    );
}
