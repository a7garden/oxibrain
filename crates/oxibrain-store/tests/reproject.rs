use oxibrain_ports::{ClockPort, FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef, project_declaration};
use oxibrain_store::reproject;
use rusqlite::Connection;

fn setup() -> (Connection, FakeClock) {
    oxibrain_store::migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../src/migrations/v1.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v2.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v3.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v4.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v5.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v6.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v7.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/v8.sql"))
        .unwrap();
    oxibrain_store::registry::seed_core_v1(&conn).unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('s1', 'test', 0)",
        [],
    )
    .unwrap();
    (conn, FakeClock::new(Timestamp(1000)))
}

fn dump_beliefs(conn: &Connection) -> String {
    // Serialize beliefs table as canonical JSON for comparison.
    let mut stmt = conn
        .prepare("SELECT statement_id, valid_from, valid_to, status, confidence, support_json FROM beliefs ORDER BY statement_id, valid_from")
        .unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            Ok(format!(
                "{{\"statement_id\":\"{}\",\"valid_from\":{},\"valid_to\":{},\"status\":\"{}\",\"confidence\":{},\"support_json\":{}}}",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    format!("[{}]", rows.join(","))
}

#[test]
fn reproject_preserves_beliefs() {
    let (conn, clock) = setup();

    // Declare two statements.
    let d1 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "works_on".into(),
        object: DeclObject::Entity {
            surface: "ProjectX".into(),
            ty: "Project".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };
    project_declaration(&conn, "s1", &d1, clock.now()).unwrap();

    clock.advance(100);
    let d2 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Bob".into(),
            ty: "Person".into(),
        },
        predicate: "works_on".into(),
        object: DeclObject::Entity {
            surface: "ProjectY".into(),
            ty: "Project".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };
    project_declaration(&conn, "s1", &d2, clock.now()).unwrap();

    let before = dump_beliefs(&conn);

    // Reproject.
    reproject::reproject(&conn).unwrap();

    let after = dump_beliefs(&conn);

    assert_eq!(
        before, after,
        "beliefs must be byte-identical after reproject"
    );
}

#[test]
fn reproject_preserves_entities() {
    let (conn, clock) = setup();

    let d1 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "works_on".into(),
        object: DeclObject::Entity {
            surface: "ProjectX".into(),
            ty: "Project".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };
    project_declaration(&conn, "s1", &d1, clock.now()).unwrap();

    let entity_count_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();

    reproject::reproject(&conn).unwrap();

    let entity_count_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();

    assert_eq!(entity_count_before, entity_count_after);
}
