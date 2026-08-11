use oxibrain_store::{migration, schema::LEDGER_SCHEMA_VERSION};
use rusqlite::Connection;

#[test]
fn migrates_from_empty_to_current() {
    let conn = Connection::open_in_memory().unwrap();
    // simulate a pre-migration db
    conn.execute_batch("CREATE TABLE spaces(id TEXT);").unwrap(); // arbitrary pre-existing content
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    // episodes table now exists
    let _n: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
}
