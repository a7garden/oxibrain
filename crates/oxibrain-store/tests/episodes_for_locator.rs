//! episodes_for_locator: the full occurrence chain of one vault file.

use oxibrain_core::{SourceRef, TrustTier, content_hash, occurrence_id, source_id};
use oxibrain_ports::Timestamp;
use oxibrain_store::{ledger, migration};
use rusqlite::Connection;

fn setup() -> (Connection, String) {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    let space_id = ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    (conn, space_id)
}

fn register_source(conn: &Connection, space_id: &str) -> String {
    let src = ledger::SourceRow {
        id: source_id("test", "vault"),
        space: space_id.into(),
        name: "vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
        created_at: Timestamp(1000),
    };
    ledger::insert_source(conn, &src).unwrap();
    src.id
}

fn ingest_event(
    conn: &Connection,
    space_id: &str,
    source_id: &str,
    locator: &str,
    predecessor: Option<&str>,
    content: &str,
) -> String {
    let ch = content_hash(content);
    let occ = occurrence_id(source_id, locator, predecessor, &ch);
    let att = ledger::IngestAttachment {
        source_id: source_id.into(),
        occurrence_id: occ.clone(),
        accepted_at: Timestamp(2000),
        principal: "test".into(),
        claims_json: "{}".into(),
    };
    let mut ep = oxibrain_core::Episode {
        id: Default::default(),
        space: space_id.to_string(),
        seq: 0,
        content_hash: ch,
        content: content.to_string(),
        source: SourceRef::Note {
            path: locator.to_string(),
        },
        trust: TrustTier::Trusted,
        kind: oxibrain_core::EpisodeKind::Primary,
        occurred_at: Timestamp(3000),
        ingested_at: Timestamp(3000),
        redacted_at: None,
    };
    ledger::insert_event(conn, &mut ep, Some(&att)).unwrap();
    occ
}

/// A → B → A ingests three episodes; the query must return all three,
/// oldest first, with the full content of each and distinct ids.
#[test]
fn episodes_for_locator_returns_full_chain_oldest_first() {
    let (conn, space_id) = setup();
    let src = register_source(&conn, &space_id);

    let occ_a1 = ingest_event(&conn, &space_id, &src, "note.md", None, "version A");
    let occ_b = ingest_event(
        &conn,
        &space_id,
        &src,
        "note.md",
        Some(&occ_a1),
        "version B",
    );
    ingest_event(&conn, &space_id, &src, "note.md", Some(&occ_b), "version A");
    // A sibling locator must not leak into the chain.
    ingest_event(&conn, &space_id, &src, "other.md", None, "unrelated");

    let eps = ledger::episodes_for_locator(&conn, &space_id, &src, "note.md").unwrap();
    assert_eq!(eps.len(), 3, "A→B→A must yield three episodes");
    let contents: Vec<&str> = eps.iter().map(|e| e.content.as_str()).collect();
    assert_eq!(contents, vec!["version A", "version B", "version A"]);
    // Reversion is not deduplicated: first and third episodes differ by id.
    assert_ne!(
        eps[0].id, eps[2].id,
        "temporal reversion must be a new event"
    );
}

#[test]
fn episodes_for_locator_empty_for_unknown_locator() {
    let (conn, space_id) = setup();
    let src = register_source(&conn, &space_id);
    let eps = ledger::episodes_for_locator(&conn, &space_id, &src, "nope.md").unwrap();
    assert!(eps.is_empty());
}

#[test]
fn episodes_for_locator_excludes_redacted() {
    let (conn, space_id) = setup();
    let src = register_source(&conn, &space_id);
    ingest_event(&conn, &space_id, &src, "a.md", None, "content");

    // Redact the episode (same direct-SQL shape as locator_states' test).
    conn.execute(
        "UPDATE episodes SET redacted_at = 3000 WHERE source_id = ?1",
        rusqlite::params![src],
    )
    .unwrap();

    let eps = ledger::episodes_for_locator(&conn, &space_id, &src, "a.md").unwrap();
    assert!(eps.is_empty(), "redacted episodes must be excluded");
}
