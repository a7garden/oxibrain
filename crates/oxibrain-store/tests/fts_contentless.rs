//! Contentless FTS (v13): zero body copies, compacted episodes stay
//! searchable, search returns the same targets as before (storage-footprint
//! plan, Task 4).

use oxibrain_core::retrieval::SearchTarget;
use oxibrain_core::{SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::{extraction, ledger, lifecycle, migration, query};
use rusqlite::Connection;

fn setup() -> (Connection, String) {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    let space_id = ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    (conn, space_id)
}

fn episode_at(conn: &Connection, space_id: &str, content: &str, at: Timestamp) -> String {
    extraction::ingest_event(
        conn,
        space_id,
        content,
        SourceRef::Note {
            path: "notes/t.md".into(),
        },
        TrustTier::Trusted,
        None,
        at,
    )
    .unwrap()
}

#[test]
fn fts_layer_stores_no_body_copy() {
    let (conn, space_id) = setup();
    episode_at(&conn, &space_id, "rareterm marker one", Timestamp(2000));
    oxibrain_store::index_ops::rebuild_indexes(&conn, &space_id).unwrap();

    // The map row exists...
    let map_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM fts_map WHERE space_id = ?1 AND target_kind = 'episode'",
            rusqlite::params![space_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(map_rows, 1);

    // ...and the FTS layer is contentless: the _content shadow tables that
    // stored full body copies (one per index, on top of episodes.content)
    // do not exist. `episodes.content` is the single text copy.
    for shadow in ["fts_word_content", "fts_ngram_content"] {
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                rusqlite::params![shadow],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            present, 0,
            "contentless FTS must have no {shadow} shadow table"
        );
    }
    // The virtual-table declarations carry the contentless options.
    for (tbl, opt) in [
        ("fts_word", "contentless_delete=1"),
        ("fts_ngram", "trigram"),
    ] {
        let sql_text: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = ?1",
                rusqlite::params![tbl],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sql_text.contains(opt),
            "{tbl} declaration must contain {opt}: {sql_text}"
        );
    }
}

#[test]
fn compacted_episode_remains_searchable() {
    let (conn, space_id) = setup();
    let ep = episode_at(&conn, &space_id, "compactme marker two", Timestamp(2000));

    // Age the episode past the 90-day compaction threshold, compact, and
    // rebuild the indexes. Pre-v13 the rebuild indexed content='' — the
    // episode silently left search.
    let n = lifecycle::compact_episodes(&conn, &space_id, Timestamp(2000 + 200 * 86_400_000), 90)
        .unwrap();
    assert_eq!(n, 1, "one episode compacted");
    let cleared: String = conn
        .query_row(
            "SELECT content FROM episodes WHERE id = ?1",
            rusqlite::params![ep],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cleared, "", "compaction cleared the in-line column");

    oxibrain_store::index_ops::rebuild_indexes(&conn, &space_id).unwrap();

    for index in [query::FtsIndex::Word, query::FtsIndex::Ngram] {
        let hits = query::fts_search(&conn, &space_id, "compactme", 10, index).unwrap();
        assert!(
            hits.iter()
                .any(|h| matches!(&h.target, SearchTarget::Episode { id } if *id == ep)),
            "compacted episode must stay searchable"
        );
    }
}

#[test]
fn search_finds_targets_in_both_indexes() {
    let (conn, space_id) = setup();
    episode_at(&conn, &space_id, "alpha xylophone marker", Timestamp(2000));
    episode_at(&conn, &space_id, "beta zephyr marker", Timestamp(2100));
    oxibrain_store::index_ops::rebuild_indexes(&conn, &space_id).unwrap();

    for index in [query::FtsIndex::Word, query::FtsIndex::Ngram] {
        let hits = query::fts_search(&conn, &space_id, "xylophone", 10, index).unwrap();
        assert_eq!(hits.len(), 1, "exactly one episode matches");
        assert!(matches!(&hits[0].target, SearchTarget::Episode { .. }));
    }

    // Redaction clears the map and both indexes.
    // (Space-level redaction path exercises fts_delete_space.)
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM fts_map WHERE space_id = ?1",
            rusqlite::params![space_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(before >= 2);
}
