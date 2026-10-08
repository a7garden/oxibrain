use oxibrain_ports::{ClockPort, FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{
    DeclObject, Declaration, EntityRef, ResolutionCache, project_declaration,
};
use oxibrain_store::reproject;
use rusqlite::Connection;

fn setup() -> (Connection, FakeClock) {
    oxibrain_store::migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    oxibrain_store::migration::run(&conn).unwrap();
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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d1, clock.now(), &mut cache).unwrap();

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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d2, clock.now(), &mut cache).unwrap();

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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d1, clock.now(), &mut cache).unwrap();

    let entity_count_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();

    reproject::reproject(&conn).unwrap();

    let entity_count_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
        .unwrap();

    assert_eq!(entity_count_before, entity_count_after);
}

/// Legacy RegisterPredicate def_json shape observed in the real ledger
/// (~2026-09-30 declare.sh): compact `"object": "literal:text"` string scheme
/// plus `"subjects"` instead of `subject_types`, and no examples/inverse_of/
/// deprecated_by fields. The predicate must still load and project.
#[test]
fn reproject_projects_legacy_register_predicate_shape() {
    let (conn, clock) = setup();

    let legacy_def = r#"{"name":"infra_fact","object":"literal:text","cardinality":"multi_valued","temporality":"static","invalidation":"coexist","symmetric":false,"subjects":["Artifact"],"description":"A durable infrastructure fact about this machine"}"#;
    let register = Declaration::RegisterPredicate {
        name: "infra_fact".into(),
        def_json: legacy_def.into(),
    };
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &register, clock.now(), &mut cache).unwrap();

    clock.advance(100);
    let add = Declaration::AddStatement {
        subject: EntityRef {
            surface: "workstation".into(),
            ty: "Artifact".into(),
        },
        predicate: "infra_fact".into(),
        object: DeclObject::Literal {
            literal_type: "text".into(),
            value: "brew manages packages".into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    };
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &add, clock.now(), &mut cache).unwrap();

    let beliefs_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
        .unwrap();
    assert!(beliefs_before >= 1, "declare must project a belief");

    let report = reproject::reproject(&conn).unwrap();
    assert_eq!(report.declarations_skipped, 0);

    let beliefs_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        beliefs_before, beliefs_after,
        "belief must survive reproject"
    );
}

/// A declaration episode with garbage content must be skipped and counted —
/// never abort the replay — while valid episodes still project.
#[test]
fn reproject_skips_garbage_declaration_and_continues() {
    let (conn, clock) = setup();

    // A good declaration first.
    let good = Declaration::AddStatement {
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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &good, clock.now(), &mut cache).unwrap();

    // Then a garbage declaration episode written directly to the ledger
    // (simulating a legacy/foreign writer).
    conn.execute(
        "INSERT INTO episodes (id, space_id, seq, content_hash, content, source_kind, trust, kind, occurred_at, ingested_at)
         VALUES ('ep-garbage', 's1', 999, x'00', '{\"op\":', 'declaration', 'untrusted', 'declaration', 1000, 1000)",
        [],
    )
    .unwrap();

    let report = reproject::reproject(&conn).unwrap();
    assert_eq!(report.declarations_skipped, 1, "garbage must be counted");
    assert!(
        report.declarations_replayed >= 1,
        "valid declarations must still replay"
    );

    // The garbage episode is untouched and the good statement projected.
    let garbage: String = conn
        .query_row(
            "SELECT content FROM episodes WHERE id = 'ep-garbage'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(garbage, "{\"op\":");
    let alice: i64 = conn
        .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
        .unwrap();
    assert!(alice >= 1, "valid declaration must project");
}
