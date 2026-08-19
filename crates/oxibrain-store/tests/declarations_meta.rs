//! RegisterSource and SetSourcePolicy declarations project into the
//! sources/source_policies tables and survive reproject.

use oxibrain_core::TrustTier;
use oxibrain_ports::Timestamp;
use oxibrain_store::project::{Declaration, ResolutionCache};
use oxibrain_store::{ledger, migration, project, reproject};
use rusqlite::Connection;

fn setup() -> Connection {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    conn
}

#[test]
fn register_source_declaration_creates_source_row() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();

    let decl = Declaration::RegisterSource {
        name: "my-vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
    };
    let mut cache = ResolutionCache::new();
    project::project_declaration(&conn, &space_id, &decl, Timestamp(2000), &mut cache).unwrap();

    let src = ledger::get_source_by_name(&conn, &space_id, "my-vault").unwrap();
    assert!(src.is_some(), "RegisterSource must create a sources row");
    let src = src.unwrap();
    assert_eq!(src.kind, "document_revision");
    assert_eq!(src.mode, "pull");
}

#[test]
fn set_source_policy_declaration_creates_policy_row() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();

    // First register the source.
    let reg = Declaration::RegisterSource {
        name: "vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
    };
    let mut cache = ResolutionCache::new();
    project::project_declaration(&conn, &space_id, &reg, Timestamp(2000), &mut cache).unwrap();

    // Then set its policy.
    let pol = Declaration::SetSourcePolicy {
        source_name: "vault".into(),
        trust: "semi_trusted".into(),
        effective_from: 1000,
        effective_to: None,
    };
    project::project_declaration(&conn, &space_id, &pol, Timestamp(3000), &mut cache).unwrap();
    let src = ledger::get_source_by_name(&conn, &space_id, "vault")
        .unwrap()
        .unwrap();
    let trust = ledger::effective_policy_trust(&conn, &src.id, Timestamp(2000)).unwrap();
    assert_eq!(trust, Some(TrustTier::SemiTrusted));
}

#[test]
fn meta_declarations_survive_reproject() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
            r.get(0)
        })
        .unwrap();

    let reg = Declaration::RegisterSource {
        name: "vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
    };
    let pol = Declaration::SetSourcePolicy {
        source_name: "vault".into(),
        trust: "trusted".into(),
        effective_from: 0,
        effective_to: None,
    };
    let mut cache = ResolutionCache::new();
    project::project_declaration(&conn, &space_id, &reg, Timestamp(2000), &mut cache).unwrap();
    project::project_declaration(&conn, &space_id, &pol, Timestamp(3000), &mut cache).unwrap();

    // Reproject wipes projection tables and replays.
    reproject::reproject(&conn).unwrap();

    // Source and policy must still exist after replay.
    let src = ledger::get_source_by_name(&conn, &space_id, "vault").unwrap();
    assert!(src.is_some(), "source must survive reproject");
    let src = src.unwrap();
    let trust = ledger::effective_policy_trust(&conn, &src.id, Timestamp(5000)).unwrap();
    assert_eq!(
        trust,
        Some(TrustTier::Trusted),
        "policy must survive reproject"
    );
}
