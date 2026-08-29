//! Contentless FTS (v13, ADR-014): zero body copies, compacted episodes stay
//! searchable, search results identical to the pre-v13 contract.

use oxibrain_ports::{ClockPort, FakeClock, Timestamp};
use oxibrain_store::index_ops::{rebuild_indexes, rebuild_tfidf};
use oxibrain_store::lifecycle::compact_episodes;
use oxibrain_store::project::{
    DeclObject, Declaration, EntityRef, ResolutionCache, project_declaration,
};
use oxibrain_store::query::fts_search;
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

/// Ingest a primary episode directly through the ledger (like the ingest
/// path does) and index it.
fn ingest(conn: &Connection, clock: &FakeClock, content: &str) -> String {
    let space = "s1";
    let now = clock.now();
    let mut ep = oxibrain_core::Episode {
        id: String::new(),
        space: space.into(),
        seq: 0,
        content_hash: oxibrain_core::content_hash(content),
        content: content.into(),
        source: oxibrain_core::SourceRef::Note {
            path: "t.md".into(),
        },
        trust: oxibrain_core::TrustTier::Trusted,
        kind: oxibrain_core::EpisodeKind::Primary,
        occurred_at: now,
        ingested_at: now,
        redacted_at: None,
    };
    oxibrain_store::ledger::insert_episode(conn, &mut ep).unwrap();
    oxibrain_store::index_ops::index_episode_fts(conn, space, &ep.id, content).unwrap();
    ep.id
}

fn declare(conn: &Connection, clock: &FakeClock, person: &str, org: &str) {
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
        valid_from: 0,
        valid_to: Timestamp(4102444800000).millis(),
    };
    let mut cache = ResolutionCache::new();
    project_declaration(conn, "s1", &decl, clock.now(), &mut cache).unwrap();
}

#[test]
fn fts_store_keeps_no_body_copy() {
    let conn = setup();
    let clock = FakeClock::new(Timestamp(1000));
    let _id = ingest(&conn, &clock, "rareterm marker one");

    // The contentless table exposes no unindexed columns: selecting the old
    // `space_id` column must fail (it no longer exists).
    let r = conn.prepare("SELECT space_id FROM fts_word");
    assert!(
        r.is_err(),
        "fts_word must be contentless (no space_id column)"
    );

    // The map row exists and binds the episode.
    let (map_rows, episodes): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM fts_map),
                    (SELECT COUNT(*) FROM fts_map WHERE target_kind = 'episode')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(map_rows >= 1);
    assert_eq!(episodes, 1, "exactly one episode map row");

    // Search still finds the episode through the contentless index.
    let hits = fts_search(
        &conn,
        "s1",
        "rareterm",
        10,
        oxibrain_store::query::FtsIndex::Word,
    )
    .unwrap();
    assert!(!hits.is_empty(), "word index must match");
    let hits = fts_search(
        &conn,
        "s1",
        "rareterm",
        10,
        oxibrain_store::query::FtsIndex::Ngram,
    )
    .unwrap();
    assert!(!hits.is_empty(), "ngram index must match");
}

#[test]
fn compacted_episode_remains_searchable() {
    let conn = setup();
    let clock = FakeClock::new(Timestamp(1000));
    let id = ingest(&conn, &clock, "compactme marker two");

    // Compact everything (ingested_at 1000 < cutoff for any age here).
    let n = compact_episodes(&conn, "s1", Timestamp(86_400_000 * 365), 1).unwrap();
    assert_eq!(n, 1, "the episode compacted");

    // Full rebuild reads the effective text (compacted payload).
    rebuild_indexes(&conn, "s1").unwrap();

    let hits = fts_search(
        &conn,
        "s1",
        "compactme",
        10,
        oxibrain_store::query::FtsIndex::Word,
    )
    .unwrap();
    assert!(
        hits.iter().any(|h| matches!(
            &h.target,
            oxibrain_core::SearchTarget::Episode { id: eid } if eid == &id
        )),
        "compacted episode must remain searchable, got {hits:?}"
    );
    assert_eq!(hits[0].score, hits[0].score, "score present (sanity)");
}

#[test]
fn search_finds_targets_after_reproject() {
    let conn = setup();
    let clock = FakeClock::new(Timestamp(1000));
    ingest(&conn, &clock, "primary source text for search");
    declare(&conn, &clock, "Alice", "Acme");
    declare(&conn, &clock, "Bob", "Globex");

    // Reproject: the ranking half rebuilds from the ledger.
    // (reproject() is exercised via rebuild here for unit scope; the
    // byte-identical contract lives in m2_index_determinism.)
    rebuild_indexes(&conn, "s1").unwrap();
    rebuild_tfidf(&conn, "s1", 1024).unwrap();

    let hits = fts_search(
        &conn,
        "s1",
        "Alice",
        10,
        oxibrain_store::query::FtsIndex::Word,
    )
    .unwrap();
    assert!(
        hits.iter()
            .any(|h| matches!(&h.target, oxibrain_core::SearchTarget::Entity { .. })),
        "entity target found, got {hits:?}"
    );

    let hits = fts_search(
        &conn,
        "s1",
        "primary source text",
        10,
        oxibrain_store::query::FtsIndex::Ngram,
    )
    .unwrap();
    assert!(
        hits.iter()
            .any(|h| matches!(&h.target, oxibrain_core::SearchTarget::Episode { .. })),
        "episode target found via ngram"
    );
}

#[test]
fn v13_migration_deletes_orphan_sources() {
    // Build a store at v12 (pre-contentless), register an orphan source and
    // a source bound to an episode, then run to CURRENT: the orphan is
    // deleted by the v13 SQL, the referenced row stays (provenance, P2).
    use oxibrain_store::migration;
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    for v in 1..=12 {
        let sql = std::fs::read_to_string(format!(
            "{}/src/migrations/v{v}.sql",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        if v == 5 || v == 7 {
            migration::ensure_vec_extension();
        }
        conn.execute_batch(&sql).unwrap();
        conn.pragma_update(None, "user_version", v).unwrap();
    }
    oxibrain_store::registry::seed_core_v1(&conn).unwrap();
    conn.execute(
        "INSERT INTO spaces (id, name, created_at) VALUES ('s1', 'test', 0)",
        [],
    )
    .unwrap();
    // Orphan: never bound to an episode.
    conn.execute(
        "INSERT INTO sources (id, space_id, name, kind, mode, claims_json, created_at)
         VALUES ('orphan', 's1', 'ghost', 'note', 'push', '{}', 0)",
        [],
    )
    .unwrap();
    // Bound: gets an episode referencing it.
    let content = "bound source text";
    let mut ep = oxibrain_core::Episode {
        id: String::new(),
        space: "s1".into(),
        seq: 0,
        content_hash: oxibrain_core::content_hash(content),
        content: content.into(),
        source: oxibrain_core::SourceRef::Note {
            path: "t.md".into(),
        },
        trust: oxibrain_core::TrustTier::Trusted,
        kind: oxibrain_core::EpisodeKind::Primary,
        occurred_at: Timestamp(1000),
        ingested_at: Timestamp(1000),
        redacted_at: None,
    };
    oxibrain_store::ledger::insert_episode(&conn, &mut ep).unwrap();
    conn.execute(
        "INSERT INTO sources (id, space_id, name, kind, mode, claims_json, created_at)
         VALUES ('bound', 's1', 'real', 'note', 'push', '{}', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE episodes SET source_id = 'bound' WHERE id = ?1",
        rusqlite::params![ep.id],
    )
    .unwrap();

    migration::run(&conn).unwrap();

    let orphans: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sources s WHERE s.id NOT IN
             (SELECT source_id FROM episodes WHERE source_id IS NOT NULL)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0, "orphan sources are gone");
    let kept: i64 = conn
        .query_row("SELECT COUNT(*) FROM sources WHERE id = 'bound'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kept, 1, "referenced sources stay (provenance, P2)");
}
