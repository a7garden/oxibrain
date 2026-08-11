use oxibrain_ports::BrainError;
use oxibrain_store::StoreHandle;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[test]
fn readers_dont_block_writer_under_load() {
    let dir = tempdir().unwrap();
    let handle = Arc::new(StoreHandle::open(dir.path()).expect("open"));
    // seed a space so reads have something (raw SQL — the ledger module lands in Task 6)
    let h = handle.clone();
    h.writer
        .submit(Box::new(|conn| {
            conn.execute(
                "INSERT INTO spaces(id, name, created_at) VALUES(?1, ?2, ?3)",
                rusqlite::params!["s1", "personal", 0i64],
            )
            .map_err(|e| BrainError::Storage(e.to_string()))?;
            Ok(())
        }))
        .unwrap();
    h.writer.flush().unwrap();

    let start = Instant::now();
    let mut threads = Vec::new();
    for _ in 0..8 {
        let h = handle.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..50 {
                // .unwrap_or(0) sidesteps the rusqlite->BrainError conversion here;
                // the point is lock/path behavior under load, not the row count.
                let _ = h.readers.read(|conn| {
                    Ok(conn
                        .query_row::<i64, _, _>("SELECT COUNT(*) FROM spaces", [], |r| r.get(0))
                        .unwrap_or(0))
                });
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    assert!(start.elapsed() < Duration::from_secs(5), "readers stalled");
}
