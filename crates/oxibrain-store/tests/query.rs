use oxibrain_ports::{ClockPort, FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef, project_declaration};
use oxibrain_store::query;
use rusqlite::Connection;

fn setup() -> (Connection, FakeClock) {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../src/migrations/v1.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v2.sql"))
        .unwrap();
    oxibrain_store::registry::seed_core_v1(&conn).unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('s1', 'test', 0)",
        [],
    )
    .unwrap();
    (conn, FakeClock::new(Timestamp(1000)))
}

fn declare_employed(conn: &Connection, clock: &FakeClock, person: &str, org: &str, from: i64) {
    let decl = Declaration::AddStatement {
        subject: EntityRef {
            surface: person.into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: org.into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: from,
        valid_to: TIME_MAX.millis(),
    };
    project_declaration(conn, "s1", &decl, clock.now()).unwrap();
}

#[test]
fn beliefs_for_entity_returns_current() {
    let (conn, clock) = setup();
    declare_employed(&conn, &clock, "Alice", "Acme", TIME_MIN.millis());

    // Find Alice's entity id.
    let alice_id: String = conn
        .query_row(
            "SELECT entity_id FROM entity_keys WHERE normalized = 'alice' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let beliefs = query::beliefs_for_entity(&conn, "s1", &alice_id).unwrap();
    assert_eq!(beliefs.len(), 1);
    assert_eq!(beliefs[0].status, oxibrain_core::BeliefStatus::Active);
}

#[test]
fn contradictions_finds_static_conflicts() {
    let (conn, clock) = setup();

    // born_in(Alice, Seoul)
    let d1 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "born_in".into(),
        object: DeclObject::Entity {
            surface: "Seoul".into(),
            ty: "Place".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };
    project_declaration(&conn, "s1", &d1, clock.now()).unwrap();

    // born_in(Alice, Busan) — contradiction!
    let d2 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "born_in".into(),
        object: DeclObject::Entity {
            surface: "Busan".into(),
            ty: "Place".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };
    clock.advance(100);
    project_declaration(&conn, "s1", &d2, clock.now()).unwrap();

    let contradicted = query::contradictions(&conn, "s1").unwrap();
    assert_eq!(
        contradicted.len(),
        2,
        "both born_in statements contradicted"
    );
}
