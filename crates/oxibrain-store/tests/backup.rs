use oxibrain_store::{Store, backup::online_backup};
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
