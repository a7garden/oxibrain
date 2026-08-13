//! M3 extraction integration tests: end-to-end pipeline with FakeLlmPort,
//! reproject determinism with extraction cache, and fabricated-entity rejection.

use oxibrain::Brain;
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_ports::{FakeClock, FakeLlmPort, LlmResponse, Timestamp};
use rusqlite::Connection;
use tempfile::TempDir;

fn dump_table(conn: &Connection, table: &str, columns: &str, order: &str) -> String {
    let sql = format!("SELECT {columns} FROM {table} ORDER BY {order}");
    let n_cols = columns.split(',').count();
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            let mut parts = Vec::new();
            for i in 0..n_cols {
                let val: String = match r.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Null) => "null".into(),
                    Ok(rusqlite::types::ValueRef::Integer(i)) => i.to_string(),
                    Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                    Ok(rusqlite::types::ValueRef::Text(t)) => {
                        format!("\"{}\"", String::from_utf8_lossy(t))
                    }
                    Ok(rusqlite::types::ValueRef::Blob(b)) => format!("blob({})", b.len()),
                    Err(_) => "?".into(),
                };
                parts.push(val);
            }
            Ok(parts.join(","))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.join(";")
}

fn dump_all(conn: &Connection) -> String {
    let mut out = String::new();
    for (table, cols, order) in [
        (
            "entities",
            "id, space_id, type_name, canonical_key, created_at, merged_into",
            "id",
        ),
        (
            "entity_keys",
            "id, space_id, entity_id, type_name, normalized, surface, origin",
            "id",
        ),
        (
            "statements",
            "id, space_id, subject_id, predicate, object_entity, object_literal",
            "id",
        ),
        (
            "assertions",
            "id, statement_id, episode_id, extractor_id, polarity, claimed_from, claimed_to, confidence, recorded_at",
            "id",
        ),
        (
            "mentions",
            "id, assertion_id, role, surface, span_start, span_end, resolved_to, method",
            "id",
        ),
        (
            "beliefs",
            "statement_id, valid_from, valid_to, status, confidence, support_json",
            "statement_id, valid_from",
        ),
    ] {
        out.push_str(&format!("--- {table} ---\n"));
        out.push_str(&dump_table(conn, table, cols, order));
        out.push('\n');
    }
    out
}

/// Canned extraction response for "Alice works on ProjectX at Acme Corp".
fn canned_response() -> &'static str {
    r#"{"claims":[
        {"predicate":"works_on","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.95},
        {"predicate":"employed_by","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"Acme Corp","entity_type":"Organization","span":[27,36]}},"polarity":"affirm","confidence":0.9}
    ]}"#
}

fn test_extractor() -> ExtractorConfig {
    ExtractorConfig {
        model_id: "test-model".into(),
        prompt_version: 1,
        registry_major: 1,
        mechanism: ExtractMechanism::JsonSchema,
        max_tokens: 4096,
        model_digest: None,
    }
}

#[tokio::test]
async fn extract_one_creates_assertions() {
    let dir = TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());

    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm = std::sync::Arc::new(FakeLlmPort::new());
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_response().into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain = Brain::with_llm(config, clock, llm).await.unwrap();
    let space = brain.ensure_space("test").await.unwrap();

    // Ingest a primary episode.
    let content = "Alice works on ProjectX at Acme Corp";
    let ep_id = brain
        .ingest(
            &space,
            content.into(),
            oxibrain_core::SourceRef::Note {
                path: "test.md".into(),
            },
            oxibrain_core::TrustTier::Trusted,
            &test_extractor().id(),
        )
        .await
        .unwrap();

    // Extract.
    let summary = brain
        .extract_one(&space, &ep_id, &test_extractor())
        .await
        .unwrap();
    assert_eq!(summary.extracted, 2, "should extract 2 claims");
    assert_eq!(summary.quarantined, 0, "no invalid claims");

    // Verify assertions exist in the DB.
    let db_path = dir.path().join("brain.db");
    let conn = Connection::open(&db_path).unwrap();
    let assertion_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assertions WHERE extractor_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        assertion_count, 2,
        "should have 2 extraction-produced assertions"
    );

    // Verify mentions have correct spans.
    let mention_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM mentions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        mention_count, 4,
        "should have 4 mentions (2 subjects + 2 objects)"
    );

    // Verify beliefs were folded.
    let belief_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
        .unwrap();
    assert!(belief_count >= 2, "should have at least 2 beliefs");
}

#[tokio::test]
async fn reproject_byte_identical_with_extraction() {
    let dir = TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());

    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm = std::sync::Arc::new(FakeLlmPort::new());
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_response().into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain = Brain::with_llm(config, clock, llm).await.unwrap();
    let space = brain.ensure_space("test").await.unwrap();

    let content = "Alice works on ProjectX at Acme Corp";
    let ep_id = brain
        .ingest(
            &space,
            content.into(),
            oxibrain_core::SourceRef::Note {
                path: "test.md".into(),
            },
            oxibrain_core::TrustTier::Trusted,
            &test_extractor().id(),
        )
        .await
        .unwrap();

    brain
        .extract_one(&space, &ep_id, &test_extractor())
        .await
        .unwrap();

    // Snapshot after incremental extraction.
    let db_path = dir.path().join("brain.db");
    let conn_before = Connection::open(&db_path).unwrap();
    let before = dump_all(&conn_before);
    drop(conn_before);

    assert!(!before.is_empty(), "projection must be non-empty");

    // Reproject.
    brain.reproject().await.unwrap();

    // Snapshot after reproject.
    let conn_after = Connection::open(&db_path).unwrap();
    let after = dump_all(&conn_after);

    assert_eq!(
        before, after,
        "projection must be byte-identical after reproject (with extraction replay)"
    );
}

#[tokio::test]
async fn fabricated_entity_rejected() {
    let dir = TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());

    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm = std::sync::Arc::new(FakeLlmPort::new());

    // Response with a fabricated entity ("Bob" not in the text at span [0,5)).
    let fabricated = r#"{"claims":[
        {"predicate":"works_on","subject":{"surface":"Bob","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.9}
    ]}"#;
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: fabricated.into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain = Brain::with_llm(config, clock, llm).await.unwrap();
    let space = brain.ensure_space("test").await.unwrap();

    let content = "Alice works on ProjectX at Acme Corp";
    let ep_id = brain
        .ingest(
            &space,
            content.into(),
            oxibrain_core::SourceRef::Note {
                path: "test.md".into(),
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
    assert_eq!(summary.extracted, 0, "fabricated entity should be rejected");
    assert_eq!(
        summary.quarantined, 1,
        "invalid claim should be quarantined"
    );
}
