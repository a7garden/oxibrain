//! Integration test: FTS5 and semantic search over a migrated DB.

use oxibrain_store::Store;
use tempfile::tempdir;

fn setup_store() -> (tempfile::TempDir, Store) {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    (dir, store)
}

// Smoke test: search functions compile and run against a migrated DB.
#[test]
fn fts_search_empty_space_returns_empty() {
    let (_dir, store) = setup_store();
    let conn = store.connection();
    let hits =
        oxibrain_store::query::fts_search(conn, "nonexistent", "test", 10).expect("fts_search");
    assert!(hits.is_empty());
}

#[test]
fn semantic_search_empty_space_returns_empty() {
    let (_dir, store) = setup_store();
    let conn = store.connection();
    let hits = oxibrain_store::query::semantic_search(conn, "nonexistent", "test", 10)
        .expect("semantic_search");
    assert!(hits.is_empty());
}
