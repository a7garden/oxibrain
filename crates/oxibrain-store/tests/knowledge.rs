use oxibrain_core::knowledge::{Entity, Object, Statement};
use oxibrain_core::registry::core_v1;
use oxibrain_ports::Timestamp;
use oxibrain_store::knowledge as kcrud;
use oxibrain_store::migration;
use oxibrain_store::registry;
use rusqlite::Connection;

fn fresh_conn() -> Connection {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().expect("open");
    migration::run(&conn).expect("migrate");
    // ensure a space exists
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('s1', 'test', 0)",
        [],
    )
    .expect("space");
    conn
}

#[test]
fn entity_round_trip() {
    let conn = fresh_conn();
    let e = Entity {
        id: "e1".into(),
        space: "s1".into(),
        ty: "Person".into(),
        canonical_key: None,
        created_at: Timestamp(100),
        merged_into: None,
    };
    kcrud::insert_entity(&conn, &e).unwrap();
    let loaded = kcrud::get_entity(&conn, "e1").unwrap().expect("found");
    assert_eq!(loaded.ty, "Person");
    assert_eq!(loaded.created_at, Timestamp(100));
}

#[test]
fn statement_assertion_round_trip() {
    let conn = fresh_conn();
    // Insert entities first (FK)
    for id in &["e1", "e2"] {
        kcrud::insert_entity(
            &conn,
            &Entity {
                id: (*id).into(),
                space: "s1".into(),
                ty: "Person".into(),
                canonical_key: None,
                created_at: Timestamp(0),
                merged_into: None,
            },
        )
        .unwrap();
    }

    let stmt = Statement {
        id: "st1".into(),
        space: "s1".into(),
        subject: "e1".into(),
        predicate: "knows".into(),
        object: Object::Entity("e2".into()),
    };
    kcrud::insert_statement(&conn, &stmt).unwrap();

    let group = kcrud::get_statement_group(&conn, "s1", "e1", "knows").unwrap();
    assert_eq!(
        group.len(),
        0,
        "no assertions yet → group excludes empty statements"
    );
}

#[test]
fn registry_seed_and_load() {
    let conn = fresh_conn();
    registry::seed_core_v1(&conn).unwrap();
    let def = registry::load_predicate(&conn, "employed_by")
        .unwrap()
        .expect("employed_by exists");
    assert_eq!(def.name, "employed_by");

    let all = registry::load_all_predicates(&conn).unwrap();
    assert_eq!(all.len(), core_v1().len());

    // Idempotent: seeding again is a no-op.
    registry::seed_core_v1(&conn).unwrap();
    let all2 = registry::load_all_predicates(&conn).unwrap();
    assert_eq!(all.len(), all2.len());
}
