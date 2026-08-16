use oxibrain_ports::{ClockPort, FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{
    DeclObject, Declaration, EntityRef, ResolutionCache, project_declaration,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn setup() -> (TempDir, Connection, FakeClock) {
    let dir = TempDir::new().unwrap();
    // Full migration set — the store project_declaration runs against in
    // production. (v1+v2 alone would leave the ranking-half tables absent,
    // and declarations now refresh entity FTS rows incrementally.)
    oxibrain_store::migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    oxibrain_store::migration::run(&conn).unwrap();
    oxibrain_store::registry::seed_core_v1(&conn).unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('s1', 'test', 0)",
        [],
    )
    .unwrap();
    let clock = FakeClock::new(Timestamp(1000));
    (dir, conn, clock)
}

#[test]
fn declare_statement_creates_belief() {
    let (_dir, conn, clock) = setup();

    let decl = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: "Acme".into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };

    let mut cache = ResolutionCache::new();
    let ep_id = project_declaration(&conn, "s1", &decl, clock.now(), &mut cache).unwrap();
    assert!(!ep_id.is_empty());

    // Check a belief was created.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "one belief for one assertion");
}

#[test]
fn supersession_updates_beliefs() {
    let (_dir, conn, clock) = setup();

    // Declare employed_by(Alice, Acme)
    let d1 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: "Acme".into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 100,
        valid_to: TIME_MAX.millis(),
    };
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d1, clock.now(), &mut cache).unwrap();

    // Declare employed_by(Alice, Globex) — should supersede Acme.
    let d2 = Declaration::AddStatement {
        subject: EntityRef {
            surface: "Alice".into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: "Globex".into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 200,
        valid_to: TIME_MAX.millis(),
    };
    clock.advance(100);
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d2, clock.now(), &mut cache).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "two beliefs: superseded + active");

    // Check statuses.
    let statuses: Vec<String> = conn
        .prepare("SELECT status FROM beliefs ORDER BY valid_from")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(statuses.contains(&"superseded".to_string()));
    assert!(statuses.contains(&"active".to_string()));
}
