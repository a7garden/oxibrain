use oxibrain::{Brain, BrainConfig};

#[tokio::test]
async fn session_record_and_recall_layer() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("personal").await.unwrap();
    brain
        .record_session(
            &space_id,
            "sess-1",
            "Discussed the florb widget redesign.".into(),
            oxibrain_ports::Timestamp::from_millis(1_700_000_000_000),
        )
        .await
        .unwrap();
    brain
        .record_session(
            &space_id,
            "sess-2",
            "Planned the 2027 roadmap offsite.".into(),
            oxibrain_ports::Timestamp::from_millis(1_700_100_000_000),
        )
        .await
        .unwrap();
    let listed = brain.list_sessions(&space_id, 10, None).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].session_id, "sess-2");

    let hints = oxibrain::RecallHints {
        session_id: Some("sess-2".into()),
        ..Default::default()
    };
    let ctx = brain
        .assemble_context_with_hints(&space_id, "roadmap", 3000, &hints)
        .await
        .unwrap();
    let listed_excl = brain
        .list_sessions(&space_id, 10, Some("sess-2"))
        .await
        .unwrap();
    assert_eq!(listed_excl.len(), 1, "excluded list: {listed_excl:?}");
    let rendered = serde_json::to_string(&ctx.layers).unwrap();
    assert!(
        !rendered.contains("sess-2"),
        "current session leaked: {rendered}"
    );
    assert!(
        rendered.contains("sess-1"),
        "prior session missing: {rendered}"
    );
}

#[tokio::test]
async fn generated_session_is_untrusted_and_conflicting_replay_cannot_replace_it() {
    use oxibrain_core::{EpisodeKind, SourceRef, TrustTier};
    use oxibrain_ports::{BrainError, Timestamp};

    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("personal").await.unwrap();
    let summary = "Agent-generated summary, not an owner declaration.";
    let at = Timestamp::from_millis(1_700_000_000_000);
    let first = brain
        .record_session(&space, "stable-session", summary.into(), at)
        .await
        .unwrap();
    let replay = brain
        .record_session(&space, "stable-session", summary.into(), at)
        .await
        .unwrap();
    assert_eq!(replay, first);
    let conflict = brain
        .record_session(&space, "stable-session", "Changed summary.".into(), at)
        .await;
    assert!(matches!(conflict, Err(BrainError::Conflict(_))));
    let episode = brain.get_episode(&first).await.unwrap().unwrap();
    assert_eq!(episode.content, summary);
    assert_eq!(episode.trust, TrustTier::Untrusted);
    assert_eq!(episode.kind, EpisodeKind::Primary);
    assert_eq!(
        episode.source,
        SourceRef::Session {
            session_id: "stable-session".into()
        }
    );
    assert_eq!(brain.episode_count_for_space(&space).await.unwrap(), 1);
}
