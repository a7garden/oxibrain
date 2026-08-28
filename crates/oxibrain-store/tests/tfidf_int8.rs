//! tfidf int8 storage roundtrip: 1 byte per dim, cosine ordering preserved,
//! lexical-vector search still ranks the matching episode first
//! (storage-footprint plan, Task 2).

use oxibrain_core::retrieval::SearchTarget;
use oxibrain_core::{SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::{extraction, ledger, migration, query};
use rusqlite::Connection;

fn setup() -> (Connection, String) {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    let space_id = ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    (conn, space_id)
}

fn episode(conn: &Connection, space_id: &str, content: &str) -> String {
    extraction::ingest_event(
        conn,
        space_id,
        content,
        SourceRef::Note {
            path: "notes/t.md".into(),
        },
        TrustTier::Trusted,
        None,
        Timestamp(2000),
    )
    .unwrap()
}

#[test]
fn tfidf_rows_are_int8_sized() {
    let (conn, space_id) = setup();
    episode(&conn, &space_id, "alpha xylophone uniqueone");
    episode(&conn, &space_id, "beta zephyr uniquetwo");
    oxibrain_store::index_ops::rebuild_tfidf(&conn, &space_id, 1024).unwrap();

    let mut stmt = conn
        .prepare("SELECT LENGTH(vector) FROM tfidf_vectors WHERE space_id = ?1")
        .unwrap();
    let lens: Vec<i64> = stmt
        .query_map(rusqlite::params![space_id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!lens.is_empty(), "rebuild must persist vectors");
    assert!(
        lens.iter().all(|&l| l == 1024),
        "one byte per dimension, got lengths {lens:?}"
    );
}

#[test]
fn lexical_search_ranks_matching_episode_first() {
    let (conn, space_id) = setup();
    let a = episode(&conn, &space_id, "alpha xylophone quasarone marker");
    let b = episode(&conn, &space_id, "beta zephyr quasartwo marker");
    oxibrain_store::index_ops::rebuild_tfidf(&conn, &space_id, 1024).unwrap();

    let hits = query::lexical_vector_search(&conn, &space_id, "xylophone", 5).unwrap();
    assert!(!hits.is_empty(), "channel must return hits");
    let first = hits
        .iter()
        .find(|h| matches!(&h.target, SearchTarget::Episode { .. }));
    assert!(
        matches!(&first.unwrap().target, SearchTarget::Episode { id } if *id == a),
        "the episode containing the query shingles must rank first"
    );
}
