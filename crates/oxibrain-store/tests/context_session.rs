//! Session-memory context tests: the SessionSummaries layer, exclusion of
//! session episodes from the RecentEpisodes layer, and list ordering/limits.

use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::context;
use oxibrain_store::ledger;
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

fn insert_episode(conn: &Connection, content: &str, source: SourceRef, occurred_at: i64) -> String {
    let mut ep = Episode {
        id: String::new(),
        space: "s1".into(),
        seq: 0,
        content_hash: ContentHash([0u8; 32]),
        content: content.into(),
        source,
        trust: TrustTier::Trusted,
        kind: EpisodeKind::Primary,
        occurred_at: Timestamp::from_millis(occurred_at),
        ingested_at: Timestamp::from_millis(occurred_at),
        redacted_at: None,
    };
    ledger::insert_episode(conn, &mut ep).unwrap();
    ep.id
}

fn session(id: &str) -> SourceRef {
    SourceRef::Session {
        session_id: id.into(),
    }
}

#[test]
fn session_layer_lists_newest_first_with_limit_and_exclusion() {
    let conn = setup();
    insert_episode(&conn, "older summary", session("s-a"), 1_000);
    insert_episode(&conn, "newer summary", session("s-b"), 2_000);
    insert_episode(&conn, "newest summary", session("s-c"), 3_000);
    insert_episode(
        &conn,
        "a plain note",
        SourceRef::Note { path: "n".into() },
        4_000,
    );

    let all = context::list_session_episodes(&conn, "s1", 10, None).unwrap();
    assert_eq!(
        all.iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["s-c", "s-b", "s-a"]
    );

    let limited = context::list_session_episodes(&conn, "s1", 2, None).unwrap();
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].session_id, "s-c");

    let excluded = context::list_session_episodes(&conn, "s1", 10, Some("s-c")).unwrap();
    assert_eq!(
        excluded
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["s-b", "s-a"]
    );
}

#[test]
fn assembled_context_has_session_layer_and_recent_excludes_sessions() {
    let conn = setup();
    insert_episode(
        &conn,
        "note one",
        SourceRef::Note { path: "n".into() },
        1_000,
    );
    insert_episode(&conn, "session summary text", session("s-old"), 2_000);

    let input = context::build_context_input(&conn, "s1", "anything", None).unwrap();

    // Session episode has its own layer…
    assert_eq!(input.session_summaries.len(), 1);
    assert_eq!(input.session_summaries[0].session_id, "s-old");
    assert_eq!(input.session_summaries[0].summary, "session summary text");
    // …and is NOT double-counted in the recent-episodes layer.
    assert!(
        input
            .episodes
            .iter()
            .all(|e| e.episode_id != input.session_summaries[0].episode_id)
    );
    assert_eq!(input.episodes.len(), 1);

    // Packing emits a SessionSummaries layer after RecentEpisodes.
    struct CountTokenizer;
    impl oxibrain_ports::TokenizerPort for CountTokenizer {
        fn count(&self, text: &str) -> usize {
            text.chars().count().div_ceil(4)
        }
        fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
            let take = max_tokens.saturating_mul(4).min(text.chars().count());
            text.chars().take(take).collect()
        }
        fn id(&self) -> &str {
            "test-count"
        }
    }
    let result =
        context::assemble_context(&conn, "s1", "anything", 3000, None, &CountTokenizer).unwrap();
    let kinds: Vec<_> = result.layers.iter().map(|l| l.kind).collect();
    let recent = kinds
        .iter()
        .position(|k| matches!(k, oxibrain_core::context::LayerKind::RecentEpisodes))
        .expect("recent episodes layer present");
    let session = kinds
        .iter()
        .position(|k| matches!(k, oxibrain_core::context::LayerKind::SessionSummaries))
        .expect("session summaries layer present");
    assert!(session > recent);
    let layer = &result.layers[session];
    assert!(layer.text.contains("[session s-old "));
    assert!(layer.text.contains("session summary text"));
}
