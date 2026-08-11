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
