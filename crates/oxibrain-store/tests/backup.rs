use oxibrain_store::{
    Store,
    backup::{inspect_snapshot, online_backup, prune_snapshot},
};
use tempfile::tempdir;

#[test]
fn online_backup_produces_readable_copy() {
    let src_dir = tempdir().unwrap();
    let store = Store::open(src_dir.path()).unwrap();
    // seed
    store
        .connection()
        .execute(
            "INSERT INTO spaces(id, name, created_at) VALUES('s1','personal',0)",
            [],
        )
        .unwrap();
    let dest = src_dir.path().join("backup.db");
    online_backup(store.connection(), &dest).unwrap();
    // read back
    let r = rusqlite::Connection::open(&dest).unwrap();
    let name: String = r
        .query_row("SELECT name FROM spaces WHERE id='s1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(name, "personal");
}

#[test]
fn read_only_online_backup_and_cache_pruning_leave_the_live_ledger_untouched() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let live = Store::open(&source).unwrap();
    live.connection()
        .execute(
            "INSERT INTO spaces(id, name, created_at) VALUES('s1','personal',0)",
            [],
        )
        .unwrap();
    live.connection()
        .execute(
            "INSERT INTO episodes
             (id, space_id, seq, content_hash, content, source_kind, trust, kind, occurred_at, ingested_at)
             VALUES('e1','s1',1,x'01','memory','test','trusted','primary',0,0)",
            [],
        )
        .unwrap();
    live.connection()
        .execute(
            "INSERT INTO extractions(episode_id, extractor_id, response_hash, raw_response, created_at)
             VALUES('e1','extractor',x'02','{}',0)",
            [],
        )
        .unwrap();
    live.connection()
        .execute(
            "INSERT INTO entities(id, space_id, type_name, created_at) VALUES('entity','s1','Thing',0)",
            [],
        )
        .unwrap();
    live.connection()
        .execute("INSERT INTO fts_word(rowid, body) VALUES(1, 'memory')", [])
        .unwrap();
    live.connection()
        .execute("INSERT INTO fts_ngram(rowid, body) VALUES(1, 'memory')", [])
        .unwrap();
    live.connection()
        .execute(
            "INSERT INTO fts_map(rowid, space_id, target_kind, target_id)
             VALUES(1, 's1', 'episode', 'e1')",
            [],
        )
        .unwrap();
    let reader = Store::open_read_only(&source).unwrap();
    let snapshot = root.path().join("snapshot.db");
    online_backup(&reader, &snapshot).unwrap();
    prune_snapshot(&snapshot, false, true).unwrap();
    inspect_snapshot(&snapshot).unwrap();

    let copy = rusqlite::Connection::open(&snapshot).unwrap();
    let count = |table: &str| -> i64 {
        copy.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    assert_eq!(count("episodes"), 1);
    assert_eq!(count("extractions"), 0);
    assert_eq!(count("entities"), 0);
    let live_extractions: i64 = live
        .connection()
        .query_row("SELECT COUNT(*) FROM extractions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(live_extractions, 1);

    let projection_free = root.path().join("projection-free.db");
    online_backup(&reader, &projection_free).unwrap();
    prune_snapshot(&projection_free, true, false).unwrap();
    inspect_snapshot(&projection_free).unwrap();
    let copy = rusqlite::Connection::open(&projection_free).unwrap();
    let extraction_count: i64 = copy
        .query_row("SELECT COUNT(*) FROM extractions", [], |row| row.get(0))
        .unwrap();
    let entity_count: i64 = copy
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(extraction_count, 1);
    assert_eq!(entity_count, 0);
}
