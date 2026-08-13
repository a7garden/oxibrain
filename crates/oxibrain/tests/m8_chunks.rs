//! M8 §8.11 chunks-table population: the projection hook that fills `chunks`
//! from `split_into_chunks` + `render_context_prefix` on `rebuild_indexes`.
//!
//! Chunks are ranking-half derived state (§5.1) — a deterministic function of
//! episode content. This test proves the store path writes real, non-empty
//! chunk rows with valid byte spans and a context prefix.

use oxibrain::Brain;
use oxibrain_ports::{FakeClock, Timestamp};
use tempfile::tempdir;

#[tokio::test]
async fn rebuild_indexes_populates_chunks() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");

    let space = brain.ensure_space("test").await.expect("space");

    // A long multi-paragraph note — several thousand bytes so the recursive
    // splitter produces more than one chunk under the default 4 KiB policy.
    let paragraph = "Alice works at Project X on the authentication redesign. \
She met Bob in Seoul and they decided to switch from JWT to session cookies for the browser client. \
Kim agreed to own the migration and write the rollout plan. "
        .to_string();
    let mut content = String::new();
    while content.len() < 12_000 {
        content.push_str(&paragraph);
        content.push_str("\n\n");
    }

    brain
        .ingest_note(
            &space,
            "meeting.md",
            content.clone(),
            Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .expect("ingest");

    brain.rebuild_indexes(&space).await.expect("rebuild");

    // Read the chunks table directly (test-only; the store owns SQLite).
    let db = dir.path().join("brain.db");
    let conn = rusqlite::Connection::open(db).expect("open db");
    let mut stmt = conn
        .prepare("SELECT episode_id, ordinal, span_start, span_end, context FROM chunks")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect");

    assert!(
        !rows.is_empty(),
        "chunks table must be populated after rebuild"
    );

    let mut prev_end: i64 = 0;
    let mut ordinals: Vec<i64> = Vec::new();
    for (_ep, ordinal, span_start, span_end, context) in &rows {
        assert!(
            span_start < span_end,
            "chunk span must be non-empty ({span_start}..{span_end})"
        );
        assert!(
            *span_end <= content.len() as i64,
            "span_end {} exceeds content length {}",
            span_end,
            content.len()
        );
        // Chunks of one episode cover the content in non-overlapping order.
        if *span_start < prev_end {
            // Only valid across a new episode; single-episode here so spans
            // must be ordered.
            panic!("chunks must be ordered and non-overlapping");
        }
        prev_end = *span_end;
        ordinals.push(*ordinal);
        // Context prefix carries the deterministic source kind.
        assert!(
            context.contains("note"),
            "context must name the source kind: {context}"
        );
    }
    assert_eq!(ordinals[0], 0, "first chunk ordinal is 0");

    // A multi-paragraph note should split into more than one chunk.
    assert!(
        rows.len() > 1,
        "multi-paragraph content should yield multiple chunks, got {}",
        rows.len()
    );
}
