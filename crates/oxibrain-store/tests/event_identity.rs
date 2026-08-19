//! Event identity semantics: occurrence-based dedup, conflict detection,
//! and independence from content-hash dedup.

use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::{ledger, migration};
use rusqlite::Connection;

fn setup() -> Connection {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    conn
}

fn base_episode(space: &str, content: &str) -> Episode {
    Episode {
        id: String::new(),
        space: space.into(),
        seq: 0,
        content_hash: ContentHash([0u8; 32]),
        content: content.into(),
        source: SourceRef::Note {
            path: "test.md".into(),
        },
        trust: TrustTier::Trusted,
        kind: EpisodeKind::Primary,
        occurred_at: Timestamp(2000),
        ingested_at: Timestamp(2000),
        redacted_at: None,
    }
}

fn register_source(conn: &Connection, space: &str, name: &str) {
    ledger::insert_source(
        conn,
        &ledger::SourceRow {
            id: name.into(),
            space: space.into(),
            name: name.into(),
            kind: "note".into(),
            mode: "push".into(),
            claims_json: "{}".into(),
            created_at: Timestamp(1000),
        },
    )
    .unwrap();
}

fn att(source_id: &str, occurrence_id: &str) -> ledger::IngestAttachment {
    ledger::IngestAttachment {
        source_id: source_id.into(),
        occurrence_id: occurrence_id.into(),
        accepted_at: Timestamp(3000),
        principal: "test-principal".into(),
        claims_json: "{}".into(),
    }
}

#[test]
fn same_occurrence_same_bytes_is_idempotent() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();
    register_source(&conn, &space_id, "src1");

    let mut ep1 = base_episode(&space_id, "hello");
    ledger::insert_event(&conn, &mut ep1, Some(&att("src1", "occ1"))).unwrap();
    let id1 = ep1.id.clone();

    let mut ep2 = base_episode(&space_id, "hello");
    ledger::insert_event(&conn, &mut ep2, Some(&att("src1", "occ1"))).unwrap();
    assert_eq!(ep2.id, id1, "same occurrence + same bytes = idempotent");
}

#[test]
fn same_occurrence_different_bytes_is_conflict() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();
    register_source(&conn, &space_id, "src1");

    let mut ep1 = base_episode(&space_id, "hello");
    ledger::insert_event(&conn, &mut ep1, Some(&att("src1", "occ1"))).unwrap();

    let mut ep2 = base_episode(&space_id, "different content");
    let err = ledger::insert_event(&conn, &mut ep2, Some(&att("src1", "occ1"))).unwrap_err();
    assert!(matches!(err, oxibrain_ports::BrainError::Conflict(_)));
}

#[test]
fn same_bytes_different_source_creates_two_episodes() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();
    register_source(&conn, &space_id, "src_a");
    register_source(&conn, &space_id, "src_b");

    let mut ep1 = base_episode(&space_id, "identical bytes");
    ledger::insert_event(&conn, &mut ep1, Some(&att("src_a", "occ_a"))).unwrap();

    let mut ep2 = base_episode(&space_id, "identical bytes");
    ledger::insert_event(&conn, &mut ep2, Some(&att("src_b", "occ_b"))).unwrap();

    assert_ne!(
        ep1.id, ep2.id,
        "same bytes from different sources = two episodes"
    );

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn no_attachment_delegates_to_legacy_path() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();

    let mut ep1 = base_episode(&space_id, "legacy content");
    ledger::insert_event(&conn, &mut ep1, None).unwrap();

    // Same content again → legacy content-hash dedup (no-op).
    let mut ep2 = base_episode(&space_id, "legacy content");
    ledger::insert_event(&conn, &mut ep2, None).unwrap();
    assert_eq!(ep1.id, ep2.id, "legacy path deduplicates by content hash");
}

#[test]
fn source_crud_roundtrip() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();

    let row = ledger::SourceRow {
        id: oxibrain_core::source_id(&space_id, "my-vault"),
        space: space_id.clone(),
        name: "my-vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
        created_at: Timestamp(1000),
    };
    ledger::insert_source(&conn, &row).unwrap();

    // Idempotent re-insert.
    ledger::insert_source(&conn, &row).unwrap();

    let found = ledger::get_source_by_name(&conn, &space_id, "my-vault").unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, row.id);

    let all = ledger::list_sources(&conn, &space_id).unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn policy_trust_lookup_respects_effective_interval() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();

    let src = ledger::SourceRow {
        id: "src_p".into(),
        space: space_id,
        name: "policy-src".into(),
        kind: "note".into(),
        mode: "push".into(),
        claims_json: "{}".into(),
        created_at: Timestamp(1000),
    };
    ledger::insert_source(&conn, &src).unwrap();

    // Policies reference a declaration episode row.
    let mut decl = base_episode(&src.space, "policy declaration");
    ledger::insert_event(&conn, &mut decl, None).unwrap();

    let pol = ledger::PolicyRow {
        id: "pol1".into(),
        source_id: "src_p".into(),
        trust: TrustTier::SemiTrusted,
        effective_from: Timestamp(100),
        effective_to: Some(Timestamp(500)),
        declaration_ep: decl.id.clone(),
        created_at: Timestamp(100),
    };
    ledger::insert_policy(&conn, &pol).unwrap();

    // Inside interval.
    let t = ledger::effective_policy_trust(&conn, "src_p", Timestamp(200)).unwrap();
    assert_eq!(t, Some(TrustTier::SemiTrusted));

    // Outside interval.
    let t = ledger::effective_policy_trust(&conn, "src_p", Timestamp(600)).unwrap();
    assert_eq!(t, None);

    // No policy at all for unknown source.
    let t = ledger::effective_policy_trust(&conn, "unknown", Timestamp(200)).unwrap();
    assert_eq!(t, None);
}
