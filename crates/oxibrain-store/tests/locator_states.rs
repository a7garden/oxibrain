//! locator_states: latest event-path episode per locator for a source.

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
        id: String::new(),
        space: space_id.into(),
        seq: 0,
        content_hash: oxibrain_core::ContentHash([0u8; 32]),
        content: content.into(),
        source: SourceRef::Note {
            path: locator.into(),
        },
        trust: TrustTier::Trusted,
        kind: oxibrain_core::EpisodeKind::Primary,
        occurred_at: Timestamp(2000),
        ingested_at: Timestamp(2000),
        redacted_at: None,
    };
    ledger::insert_event(conn, &mut ep, Some(&att)).unwrap();
    occ
}

#[test]
fn locator_states_returns_latest_per_locator() {
    let (conn, space_id) = setup();
    let src = register_source(&conn, &space_id);

    let occ1 = ingest_event(&conn, &space_id, &src, "a.md", None, "version 1");
    let occ2 = ingest_event(&conn, &space_id, &src, "a.md", Some(&occ1), "version 2");
    ingest_event(&conn, &space_id, &src, "b.md", None, "other file");

    let states = ledger::locator_states(&conn, &space_id, &src).unwrap();
    assert_eq!(states.len(), 2);

    let a = &states["a.md"];
    assert_eq!(
        a.latest_occurrence_id, occ2,
        "must return latest occurrence"
    );
    assert_eq!(a.latest_content_hash, content_hash("version 2"));

    let b = &states["b.md"];
    assert_eq!(b.latest_content_hash, content_hash("other file"));
}

#[test]
fn locator_states_empty_for_unknown_source() {
    let (conn, space_id) = setup();
    let states = ledger::locator_states(&conn, &space_id, "nonexistent").unwrap();
    assert!(states.is_empty());
}

#[test]
fn locator_states_excludes_redacted() {
    let (conn, space_id) = setup();
    let src = register_source(&conn, &space_id);
    ingest_event(&conn, &space_id, &src, "a.md", None, "content");

    // Redact the episode.
    conn.execute(
        "UPDATE episodes SET redacted_at = 3000 WHERE source_id = ?1",
        rusqlite::params![src],
    )
    .unwrap();

    let states = ledger::locator_states(&conn, &space_id, &src).unwrap();
    assert!(states.is_empty(), "redacted episodes must be excluded");
}
