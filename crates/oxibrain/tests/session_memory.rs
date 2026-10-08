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
