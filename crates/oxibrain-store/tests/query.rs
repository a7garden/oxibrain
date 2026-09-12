use oxibrain_ports::{ClockPort, FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{
    DeclObject, Declaration, EntityRef, ResolutionCache, project_declaration,
};
use oxibrain_store::query;
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
    let mut cache = ResolutionCache::new();
    project_declaration(conn, "s1", &decl, clock.now(), &mut cache).unwrap();
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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d1, clock.now(), &mut cache).unwrap();

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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d2, clock.now(), &mut cache).unwrap();

    let contradicted = query::contradictions(&conn, "s1").unwrap();
    assert_eq!(
        contradicted.len(),
        2,
        "both born_in statements contradicted"
    );
}

#[test]
fn contradiction_details_carries_surfaces_and_episodes() {
    let (conn, clock) = setup();

    // born_in(Alice, Seoul) and born_in(Alice, Busan) — static conflict.
    // Reuse the exact fixture calls of contradictions_finds_static_conflicts
    // above this test; copy them verbatim so this test is self-contained.

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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d1, clock.now(), &mut cache).unwrap();

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
    let mut cache = ResolutionCache::new();
    project_declaration(&conn, "s1", &d2, clock.now(), &mut cache).unwrap();

    let details = query::contradiction_details(&conn, "s1").unwrap();
    assert_eq!(
        details.len(),
        2,
        "both conflicting statements appear: {details:?}"
    );
    for d in &details {
        assert_eq!(d.subject_surface, "Alice");
        assert_eq!(d.subject_type, "Person");
        assert_eq!(d.predicate, "born_in");
        assert_eq!(d.object_kind, "entity");
        assert!(
            !d.affirm_episodes.is_empty(),
            "each value names its episode"
        );
        assert!(d.object_value == "Seoul" || d.object_value == "Busan");
    }
    let values: Vec<&str> = details.iter().map(|d| d.object_value.as_str()).collect();
    assert!(values.contains(&"Seoul") && values.contains(&"Busan"));
}

fn memory_query(text: &str) -> oxibrain_core::retrieval::Query {
    use oxibrain_core::retrieval::{Query, QueryMode, SearchPlane};
    use std::collections::BTreeSet;
    Query {
        text: text.into(),
        mode: QueryMode::Lexical,
        space: "s1".into(),
        as_of: None,
        limit: 10,
        min_confidence: 0.0,
        planes: BTreeSet::from([SearchPlane::Memory]),
    }
}

#[test]
fn entity_surface_hits_carry_belief_snippet() {
    let (conn, clock) = setup();
    declare_employed(&conn, &clock, "Alice", "Acme", TIME_MIN.millis());

    let ranking = query::hybrid_query(&conn, &memory_query("Alice"), None).unwrap();
    let hits = query::search_results(&conn, "s1", &ranking).unwrap();
    let alice = hits
        .iter()
        .find(|h| h.entity_surface == "Alice")
        .expect("lexical channel must surface the Alice entity hit");
    assert_eq!(
        alice.snippet, "employed_by Acme",
        "a surface hit quotes the entity's strongest active belief: {hits:?}"
    );

    // The cue is deterministic: repeated identical queries produce the
    // identical snippet.
    for _ in 0..3 {
        let ranking = query::hybrid_query(&conn, &memory_query("Alice"), None).unwrap();
        let hits = query::search_results(&conn, "s1", &ranking).unwrap();
        let again = hits
            .iter()
            .find(|h| h.entity_surface == "Alice")
            .expect("repeat hit");
        assert_eq!(again.snippet, "employed_by Acme");
    }
}

#[test]
fn object_only_entity_keeps_empty_snippet() {
    let (conn, clock) = setup();
    declare_employed(&conn, &clock, "Alice", "Acme", TIME_MIN.millis());

    // "Acme" is only ever an object: it has no subject statements, hence no
    // active belief to quote. The cue stays empty rather than inventing one.
    let ranking = query::hybrid_query(&conn, &memory_query("Acme"), None).unwrap();
    let hits = query::search_results(&conn, "s1", &ranking).unwrap();
    let acme = hits
        .iter()
        .find(|h| h.entity_surface == "Acme")
        .expect("lexical channel must surface the Acme entity hit");
    assert_eq!(acme.snippet, "", "no active belief, no cue: {hits:?}");
}
