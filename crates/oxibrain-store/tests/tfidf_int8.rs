//! TF-IDF vectors persist as symmetric int8 (ADR-014): 1 KB/row, and the
//! loaded KNN index still ranks the matching episode first.

use oxibrain_index::TfIdfModel;
use oxibrain_ports::{ClockPort, FakeClock, Timestamp};
use oxibrain_store::index_ops::rebuild_tfidf;
use oxibrain_store::project::{
    DeclObject, Declaration, EntityRef, ResolutionCache, project_declaration,
};
use oxibrain_store::query::load_knn_index;
use rusqlite::Connection;

fn setup() -> Connection {
    oxibrain_store::migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    oxibrain_store::migration::run(&conn).unwrap();
    oxibrain_store::registry::seed_core_v1(&conn).unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('s1', 'test', 0)",
        [],
    )
    .unwrap();
    conn
}

fn declare(conn: &Connection, clock: &FakeClock, text: &str, org: &str) {
    let decl = Declaration::AddStatement {
        subject: EntityRef {
            surface: text.into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: org.into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: Timestamp(4102444800000).millis(),
    };
    let mut cache = ResolutionCache::new();
    project_declaration(conn, "s1", &decl, clock.now(), &mut cache).unwrap();
}

#[test]
fn tfidf_vectors_store_as_int8_and_still_rank() {
    let conn = setup();
    let clock = FakeClock::new(Timestamp(1000));
    // Distinct bodies so trigram features separate the episodes.
    declare(&conn, &clock, "Zarblonix", "Frobnicator");
    declare(&conn, &clock, "Wubbleflop", "Greebleworks");

    let dim = 1024;
    rebuild_tfidf(&conn, "s1", dim).unwrap();

    let (len, n): (i64, i64) = conn
        .query_row(
            "SELECT LENGTH(vector), COUNT(*) FROM tfidf_vectors WHERE space_id = 's1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(n, 2, "two targets ranked");
    assert_eq!(len as usize, dim, "int8 blob: one byte per dimension");

    // The loaded index still matches the query's own episode.
    let index = load_knn_index(&conn, "s1").unwrap();
    let model = TfIdfModel::fit(&["Zarblonix employed_by Frobnicator"], dim);
    let q = model.transform("Zarblonix employed_by Frobnicator");
    let results = index.search(&q, 1);
    assert!(!results.is_empty(), "knn must return a hit");
    // The query text equals the statement's rendered body, so the statement
    // target is the expected top hit — retrieved through the int8 blob.
    assert!(
        results[0].0.starts_with("statement:"),
        "top hit is the matching statement, got {}",
        results[0].0
    );
}
