//! Curation declaration variants: Split, Alias, RegisterPredicate.
//!
//! Each test exercises one new `Declaration` variant and verifies the
//! projected side-effects in the underlying store tables. Tests follow the
//! same pattern as `declarations_meta.rs` — a fresh in-memory DB, seed
//! `core/v1` predicates, create a space, project the declaration, and assert.

use oxibrain_ports::BrainError;
use oxibrain_ports::Timestamp;
use oxibrain_store::project::{Declaration, EntityRef, ResolutionCache, project_declaration};
use oxibrain_store::{ledger, migration, registry, reproject};
use rusqlite::Connection;

fn setup() -> Connection {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    registry::seed_core_v1(&conn).unwrap();
    ledger::create_space(&conn, "test", Timestamp(0)).unwrap();
    conn
}

fn space_id(conn: &Connection) -> String {
    conn.query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| {
        r.get(0)
    })
    .unwrap()
}

fn person_ref(surface: &str) -> EntityRef {
    EntityRef {
        surface: surface.into(),
        ty: "Person".into(),
    }
}

#[test]
fn split_undoes_active_merge() {
    let conn = setup();
    let space = space_id(&conn);
    let mut cache = ResolutionCache::new();

    // Merge "Bob" into "Alice".
    let merge = Declaration::Merge {
        loser: person_ref("Bob"),
        winner: person_ref("Alice"),
    };
    project_declaration(&conn, &space, &merge, Timestamp(1000), &mut cache).unwrap();

    // Loser (Bob) should now be merged_into the winner (Alice).
    let bob_id: String = conn
        .query_row(
            "SELECT id FROM entities WHERE merged_into IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let merged_into: Option<String> = conn
        .query_row(
            "SELECT merged_into FROM entities WHERE id = ?1",
            [&bob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(merged_into.is_some(), "merge must set merged_into");

    // Split the loser.
    let split = Declaration::Split {
        entity: person_ref("Bob"),
    };
    project_declaration(&conn, &space, &split, Timestamp(2000), &mut cache).unwrap();

    // merged_into must be cleared on the loser.
    let merged_into_after: Option<String> = conn
        .query_row(
            "SELECT merged_into FROM entities WHERE id = ?1",
            [&bob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        merged_into_after.is_none(),
        "split must clear merged_into on the loser"
    );

    // Merge record's undone_at must be set.
    let undone_at: Option<i64> = conn
        .query_row(
            "SELECT undone_at FROM entity_merges WHERE loser_id = ?1",
            [&bob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(undone_at, Some(2000), "split must set undone_at");
}

#[test]
fn split_without_merge_fails() {
    let conn = setup();
    let space = space_id(&conn);
    let mut cache = ResolutionCache::new();

    // Create an entity that is NOT part of any merge.
    let stmt = Declaration::AddStatement {
        subject: person_ref("Charlie"),
        predicate: "employed_by".into(),
        object: oxibrain_store::project::DeclObject::Entity {
            surface: "Acme".into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: i64::MAX,
    };
    project_declaration(&conn, &space, &stmt, Timestamp(1000), &mut cache).unwrap();

    // Split must fail with Invalid.
    let split = Declaration::Split {
        entity: person_ref("Charlie"),
    };
    let err = project_declaration(&conn, &space, &split, Timestamp(2000), &mut cache)
        .expect_err("split on unmerged entity must fail");
    assert!(
        matches!(err, BrainError::Invalid(_)),
        "expected BrainError::Invalid, got {err:?}"
    );
}

#[test]
fn alias_adds_user_declared_key() {
    let conn = setup();
    let space = space_id(&conn);
    let mut cache = ResolutionCache::new();

    // First create the entity via an AddStatement so the resolution path has
    // a canonical key to attach the alias to.
    let stmt = Declaration::AddStatement {
        subject: person_ref("Alice"),
        predicate: "employed_by".into(),
        object: oxibrain_store::project::DeclObject::Entity {
            surface: "Acme".into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: i64::MAX,
    };
    project_declaration(&conn, &space, &stmt, Timestamp(1000), &mut cache).unwrap();

    let alice_id: String = conn
        .query_row(
            "SELECT e.id FROM entities e
             JOIN entity_keys k ON k.entity_id = e.id
             WHERE k.surface = 'Alice' AND e.space_id = ?1",
            [&space],
            |r| r.get(0),
        )
        .unwrap();

    // Declare an alias.
    let alias = Declaration::Alias {
        entity: person_ref("Alice"),
        surface: "Al".into(),
    };
    project_declaration(&conn, &space, &alias, Timestamp(2000), &mut cache).unwrap();

    // The alias must exist as a key with UserDeclared origin on Alice.
    let row: (String, String) = conn
        .query_row(
            "SELECT origin, entity_id FROM entity_keys
             WHERE space_id = ?1 AND surface = 'Al'",
            [&space],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row.0, "user_declared", "alias must be UserDeclared origin");
    assert_eq!(row.1, alice_id, "alias must attach to the resolved entity");
}

#[test]
fn alias_is_idempotent() {
    let conn = setup();
    let space = space_id(&conn);
    let mut cache = ResolutionCache::new();

    // Bootstrap the entity.
    let stmt = Declaration::AddStatement {
        subject: person_ref("Alice"),
        predicate: "employed_by".into(),
        object: oxibrain_store::project::DeclObject::Entity {
            surface: "Acme".into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: i64::MAX,
    };
    project_declaration(&conn, &space, &stmt, Timestamp(1000), &mut cache).unwrap();

    // Declare the same alias twice.
    let alias = Declaration::Alias {
        entity: person_ref("Alice"),
        surface: "Al".into(),
    };
    project_declaration(&conn, &space, &alias, Timestamp(2000), &mut cache).unwrap();
    project_declaration(&conn, &space, &alias.clone(), Timestamp(3000), &mut cache)
        .expect("second alias must be idempotent, not an error");

    // Exactly one key for the alias surface.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entity_keys WHERE space_id = ?1 AND surface = 'Al'",
            [&space],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idempotent alias must produce exactly one key");
}

#[test]
fn register_predicate_inserts_row() {
    let conn = setup();
    let space = space_id(&conn);
    let mut cache = ResolutionCache::new();

    let def_json = r#"{
        "name": "custom_likes",
        "major_version": 2,
        "minor_version": 5,
        "subject_types": ["Person"],
        "object_kind": {"kind": "entity", "types": ["Concept"]},
        "polarity": "single",
        "temporal": "interval"
    }"#;
    let decl = Declaration::RegisterPredicate {
        name: "custom_likes".into(),
        def_json: def_json.into(),
    };
    project_declaration(&conn, &space, &decl, Timestamp(1000), &mut cache).unwrap();

    // predicates is a global table; look up the row directly.
    let row: (i64, i64) = conn
        .query_row(
            "SELECT major_version, minor_version FROM predicates WHERE name = 'custom_likes'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (2, 5), "major/minor versions extracted from def_json");
}

#[test]
fn reproject_reproduces_split() {
    let conn = setup();
    let space = space_id(&conn);
    let mut cache = ResolutionCache::new();

    // Merge, then split.
    let merge = Declaration::Merge {
        loser: person_ref("Bob"),
        winner: person_ref("Alice"),
    };
    project_declaration(&conn, &space, &merge, Timestamp(1000), &mut cache).unwrap();
    let split = Declaration::Split {
        entity: person_ref("Bob"),
    };
    project_declaration(&conn, &space, &split, Timestamp(2000), &mut cache).unwrap();

    // Capture state before reproject.
    let before_undone: Option<i64> = conn
        .query_row(
            "SELECT undone_at FROM entity_merges
             WHERE loser_id IN (SELECT id FROM entities WHERE space_id = ?1)
             ORDER BY decided_at DESC LIMIT 1",
            [&space],
            |r| r.get(0),
        )
        .unwrap();
    let before_merged_into: Option<String> = conn
        .query_row(
            "SELECT merged_into FROM entities WHERE space_id = ?1 AND merged_into IS NOT NULL",
            [&space],
            |r| r.get(0),
        )
        .ok();
    assert_eq!(
        before_undone,
        Some(2000),
        "split state visible before reproject"
    );
    assert!(
        before_merged_into.is_none(),
        "no merged-into entities after split, before reproject"
    );

    // Reproject from the declaration log.
    reproject::reproject(&conn).unwrap();

    // State after reproject must match.
    let after_undone: Option<i64> = conn
        .query_row(
            "SELECT undone_at FROM entity_merges ORDER BY decided_at DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let after_merged_into_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE space_id = ?1 AND merged_into IS NOT NULL",
            [&space],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        after_undone,
        Some(2000),
        "split undone_at survives reproject"
    );
    assert_eq!(
        after_merged_into_count, 0,
        "no entity stays merged into another after reproject"
    );
}
