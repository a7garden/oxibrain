//! M3d fast eval suite: golden corpus extraction quality test.
//! Uses FakeLlmPort with fixture-replayed responses — no network, deterministic.

use oxibrain::Brain;
use oxibrain_core::eval::{ExtractedTriple, compute_metrics};
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_ports::{FakeClock, FakeLlmPort, LlmResponse, Timestamp};
use rusqlite::Connection;
use tempfile::TempDir;

struct GoldenFixture {
    content: &'static str,
    canned_response: &'static str,
    expected_triples: Vec<ExtractedTriple>,
}

fn fixtures() -> Vec<GoldenFixture> {
    vec![
        GoldenFixture {
            content: "Alice works on ProjectX at Acme Corp",
            canned_response: r#"{"claims":[
                {"predicate":"works_on","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.95},
                {"predicate":"employed_by","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"Acme Corp","entity_type":"Organization","span":[27,36]}},"polarity":"affirm","confidence":0.9}
            ]}"#,
            expected_triples: vec![
                triple("works_on", "Alice", "ProjectX"),
                triple("employed_by", "Alice", "Acme Corp"),
            ],
        },
        GoldenFixture {
            content: "Bob knows Carol. Bob was born in Seoul.",
            canned_response: r#"{"claims":[
                {"predicate":"knows","subject":{"surface":"Bob","entity_type":"Person","span":[0,3]},"object":{"kind":"entity","mention":{"surface":"Carol","entity_type":"Person","span":[10,15]}},"polarity":"affirm","confidence":0.9},
                {"predicate":"born_in","subject":{"surface":"Bob","entity_type":"Person","span":[17,20]},"object":{"kind":"entity","mention":{"surface":"Seoul","entity_type":"Place","span":[33,38]}},"polarity":"affirm","confidence":0.95}
            ]}"#,
            expected_triples: vec![
                triple("knows", "Bob", "Carol"),
                triple("born_in", "Bob", "Seoul"),
            ],
        },
        GoldenFixture {
            content: "Alice full name is Alice Smith.",
            canned_response: r#"{"claims":[
                {"predicate":"full_name","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"literal","literal_type":"text","value":"Alice Smith","span":[18,30]},"polarity":"affirm","confidence":0.95}
            ]}"#,
            expected_triples: vec![triple("full_name", "Alice", "Alice Smith")],
        },
    ]
}

fn triple(p: &str, s: &str, o: &str) -> ExtractedTriple {
    ExtractedTriple {
        predicate: p.into(),
        subject_surface: s.into(),
        object_surface: o.into(),
    }
}

fn test_extractor() -> ExtractorConfig {
    ExtractorConfig {
        model_id: "test-model".into(),
        prompt_version: 1,
        registry_major: 1,
        mechanism: ExtractMechanism::JsonSchema,
        max_tokens: 4096,
    }
}

/// Extract triples from the brain's DB by joining assertions → mentions.
/// For literal objects (no mention), parse the statement's object_literal JSON.
fn extract_triples(conn: &Connection) -> Vec<ExtractedTriple> {
    let mut stmt = conn
        .prepare(
            "SELECT s.predicate, subj.surface, obj.surface, s.object_literal
             FROM assertions a
             JOIN statements s ON a.statement_id = s.id
             JOIN mentions subj ON subj.assertion_id = a.id AND subj.role = 'subject'
             LEFT JOIN mentions obj ON obj.assertion_id = a.id AND obj.role = 'object'",
        )
        .unwrap();
    stmt.query_map([], |r| {
        let predicate: String = r.get(0)?;
        let subject: String = r.get(1).unwrap_or_default();
        let object_mention: Option<String> = r.get(2).ok();
        let object_literal: Option<String> = r.get(3).ok();

        let object_surface = object_mention.unwrap_or_else(|| {
            object_literal
                .as_deref()
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                .and_then(|v| v.get("value").and_then(|v| v.as_str()).map(String::from))
                .unwrap_or_default()
        });

        Ok(ExtractedTriple {
            predicate,
            subject_surface: subject,
            object_surface,
        })
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

#[tokio::test]
async fn fast_eval_suite() {
    let fixtures = fixtures();
    let mut all_extracted = Vec::new();
    let mut all_expected = Vec::new();

    for fixture in &fixtures {
        let dir = TempDir::new().unwrap();
        let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());

        let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
        let llm = std::sync::Arc::new(FakeLlmPort::new());
        llm.respond_to(
            &fixture.content[..20.min(fixture.content.len())],
            LlmResponse {
                text: fixture.canned_response.into(),
                raw: serde_json::Value::Null,
            },
        );

        let brain = Brain::with_llm(config, clock, llm).await.unwrap();
        let space = brain.ensure_space("eval").await.unwrap();

        let ep_id = brain
            .ingest(
                &space,
                fixture.content.into(),
                oxibrain_core::SourceRef::Note {
                    path: "fixture.md".into(),
                },
                oxibrain_core::TrustTier::Trusted,
                &test_extractor().id(),
            )
            .await
            .unwrap();

        let summary = brain
            .extract_one(&space, &ep_id, &test_extractor())
            .await
            .unwrap();

        assert!(summary.extracted > 0, "should extract claims from fixture");

        let db_path = dir.path().join("brain.db");
        let conn = Connection::open(&db_path).unwrap();
        let extracted = extract_triples(&conn);
        all_extracted.extend(extracted);
        all_expected.extend(fixture.expected_triples.clone());
    }

    let metrics = compute_metrics(&all_extracted, &all_expected);

    eprintln!("Fast eval metrics: {:?}", metrics);
    eprintln!("Extracted triples: {:?}", all_extracted);
    eprintln!("Expected triples: {:?}", all_expected);

    let gate_result = metrics.check_gates();
    assert!(
        gate_result.is_ok(),
        "§14.2 quality gates failed: {:?}",
        gate_result
    );
}
