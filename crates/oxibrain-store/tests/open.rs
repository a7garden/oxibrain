use oxibrain_store::Store;
use tempfile::tempdir;

#[test]
fn open_creates_and_migrates() {
    let dir = tempdir().unwrap();
    let store = Store::open(dir.path()).expect("open");
    assert_eq!(store.user_version().unwrap(), 8);
    assert!(store.db_path().exists());
}

#[test]
fn second_open_in_same_process_is_locked() {
    let dir = tempdir().unwrap();
    let _first = Store::open(dir.path()).expect("first open");
    let second = Store::open(dir.path());
    assert!(matches!(
        second,
        Err(oxibrain_ports::BrainError::Locked { .. })
    ));
}
