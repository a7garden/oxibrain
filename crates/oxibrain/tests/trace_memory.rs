//! Trajectory memory (experience memory spec §2): event identity,
//! idempotent replay, conflict-on-different-bytes, per-agent sources.

use oxibrain::{Brain, BrainConfig};
use oxibrain_core::{EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{BrainError, Timestamp};

const TRACE: &str = "[step 001 · 2026-10-16T02:11:04Z · tool: bash]\naction: cargo build --release\nobservation: ok";

fn at() -> Timestamp {
    Timestamp::from_millis(1_700_000_000_000)
}

#[tokio::test]
async fn trace_replay_idempotent_and_conflict_typed() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("dev").await.unwrap();

    let first = brain
        .record_trace(
            &space,
            "claude-code",
            "run-1",
            TRACE.into(),
            at(),
            TrustTier::Untrusted,
        )
        .await
        .unwrap();
    let replay = brain
        .record_trace(
            &space,
            "claude-code",
            "run-1",
            TRACE.into(),
            at(),
            TrustTier::Untrusted,
        )
        .await
        .unwrap();
    assert_eq!(replay, first, "identical bytes must be an idempotent retry");

    let conflict = brain
        .record_trace(
            &space,
            "claude-code",
            "run-1",
            format!("{TRACE}\n\n[step 002 · 2026-10-16T02:12:00Z · tool: bash]\naction: retry\nobservation: ok"),
            at(),
            TrustTier::Untrusted,
        )
        .await;
    assert!(
        matches!(conflict, Err(BrainError::Conflict(_))),
        "same run id with different bytes must be a conflict, got {conflict:?}"
    );

    let episode = brain.get_episode(&first).await.unwrap().unwrap();
    assert_eq!(episode.kind, EpisodeKind::Primary);
    assert_eq!(episode.trust, TrustTier::Untrusted);
    assert_eq!(
        episode.source,
        SourceRef::AgentTrace {
            agent: "claude-code".into()
        }
    );
    assert_eq!(episode.content, TRACE, "original bytes preserved");
}

#[tokio::test]
async fn distinct_agents_same_run_id_stay_distinct() {
    // Event identity is (space, source, occurrence) — equal bytes from
    // independent agents must never collapse (§5.6: content_hash verifies,
    // it does not identify).
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("dev").await.unwrap();

    let a = brain
        .record_trace(
            &space,
            "agent-a",
            "run-1",
            TRACE.into(),
            at(),
            TrustTier::Untrusted,
        )
        .await
        .unwrap();
    let b = brain
        .record_trace(
            &space,
            "agent-b",
            "run-1",
            TRACE.into(),
            at(),
            TrustTier::Untrusted,
        )
        .await
        .unwrap();
    assert_ne!(a, b, "byte-equal traces from distinct agents collapsed");
}

#[tokio::test]
async fn trace_validates_agent_and_run_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("dev").await.unwrap();

    for (agent, run) in [
        ("", "run-1"),
        (" ", "run-1"),
        ("claude-code", ""),
        ("claude-code", " "),
    ] {
        let err = brain
            .record_trace(&space, agent, run, TRACE.into(), at(), TrustTier::Untrusted)
            .await;
        assert!(
            matches!(err, Err(BrainError::Invalid(_))),
            "expected Invalid for agent={agent:?} run={run:?}, got {err:?}"
        );
    }
}
