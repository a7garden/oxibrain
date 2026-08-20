use oxibrain::{Brain, BrainConfig, ClockPort};
use oxibrain_ports::SystemClock;
use tempfile::TempDir;

#[tokio::test]
async fn ingest_and_read() {
    let dir = TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("personal").await.unwrap();
    let id = brain
        .ingest_note(&space, "note.md", "hello brain".into(), SystemClock.now())
        .await
        .unwrap();
    let got = brain.get_episode(&id).await.unwrap().unwrap();
    assert_eq!(got.content, "hello brain");
    assert_eq!(brain.episode_count().await.unwrap(), 1);
}

#[tokio::test]
async fn read_only_mode_allows_reads_blocks_writes() {
    let dir = TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("personal").await.unwrap();
    let id = brain
        .ingest_note(
            &space,
            "note.md",
            "read-only test".into(),
            SystemClock.now(),
        )
        .await
        .unwrap();
    drop(brain); // release advisory lock + writer

    // Open read-only.
    let ro = Brain::open_ro(BrainConfig::at(dir.path())).await.unwrap();

    // Reads succeed.
    let ep = ro.get_episode(&id).await.unwrap().unwrap();
    assert_eq!(ep.content, "read-only test");
    assert_eq!(ro.episode_count().await.unwrap(), 1);

    // Writes fail with a clear error.
    let err = ro
        .ingest_note(&space, "x.md", "blocked".into(), SystemClock.now())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("read-only"),
        "expected read-only error, got: {err}"
    );
}

#[tokio::test]
async fn read_only_open_fails_on_missing_store() {
    let dir = TempDir::new().unwrap();
    let msg = match Brain::open_ro(BrainConfig::at(dir.path())).await {
        Ok(_) => panic!("expected error for missing store"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("no brain.db") || msg.contains("NotFound"),
        "expected not-found error, got: {msg}"
    );
}

#[tokio::test]
async fn list_spaces_returns_created_spaces() {
    let dir = TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("work").await.unwrap();
    let _ = brain.ensure_space("personal").await.unwrap();
    let spaces = brain.list_spaces().await.unwrap();
    let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"work"));
    assert!(names.contains(&"personal"));
    assert!(spaces.iter().all(|s| s.created_at.millis() > 0));
}
