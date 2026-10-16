//! Runbook consolidation (experience memory spec §4, M11): Task-anchored
//! clusters → Derived runbook episodes with Uncertainty, terminal by
//! construction (never in the extraction backlog), cached, and
//! checkpoint-idempotent on replay.

use oxibrain::{Brain, BrainConfig};
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_core::{EpisodeKind, TrustTier};
use oxibrain_ports::{FakeClock, FakeLlmPort, LlmResponse, Timestamp};
use rusqlite::Connection;
use std::sync::Arc;
use tempfile::TempDir;

const TRACE: &str = "[step 001 · 2026-10-16T02:11:04Z · tool: bash]\naction: run deploy (shop-prod)\nobservation: deploy (shop-prod) fails when the vendored crate cache is cold. deploy (shop-prod) requires the migration playbook.";

const RUNBOOK: &str = "## Runbook: deploy (shop-prod)\n### Steps\n- warm the vendored crate cache first, then build\n### Gotchas\n- the build fails when the vendored crate cache is cold\n### Broken premises\n- none observed";

fn canned_extraction() -> &'static str {
    r#"{"claims":[
  {"predicate":"fails_when",
   "subject":{"surface":"deploy (shop-prod)","entity_type":"Task","quote":"run deploy (shop-prod)"},
   "object":{"kind":"literal","literal_type":"text","value":"the vendored crate cache is cold","quote":"deploy (shop-prod) fails when the vendored crate cache is cold"},
   "polarity":"affirm","confidence":0.8},
  {"predicate":"requires",
   "subject":{"surface":"deploy (shop-prod)","entity_type":"Task","quote":"run deploy (shop-prod)"},
   "object":{"kind":"entity","mention":{"surface":"migration playbook","entity_type":"Document","quote":"requires the migration playbook"}},
   "polarity":"affirm","confidence":0.8}
]}"#
}

fn config() -> ExtractorConfig {
    ExtractorConfig {
        model_id: "test-model".into(),
        prompt_version: 2,
        registry_major: 1,
        mechanism: ExtractMechanism::JsonSchema,
        max_tokens: 4096,
        model_digest: None,
        provider_profile_id: None,
    }
}

async fn brain_with_runbook_llm(dir: &TempDir) -> (Brain, String) {
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(10_000)));
    let llm = Arc::new(FakeLlmPort::new());
    // Insertion order matters (first match wins): the runbook key matches
    // only runbook prompts; the extraction key matches the shared episode
    // text inside extraction prompts.
    llm.respond_to(
        "Write a runbook",
        LlmResponse {
            text: RUNBOOK.into(),
            raw: serde_json::Value::Null,
        },
    );
    llm.respond_to(
        "observation:",
        LlmResponse {
            text: canned_extraction().into(),
            raw: serde_json::Value::Null,
        },
    );
    let brain = Brain::with_llm(BrainConfig::at(dir.path()), clock, llm)
        .await
        .unwrap();
    let space = brain.ensure_space("dev").await.unwrap();
    (brain, space)
}

#[tokio::test]
async fn runbooks_derived_terminal_uncertain_and_idempotent() {
    let dir = TempDir::new().unwrap();
    let (brain, space) = brain_with_runbook_llm(&dir).await;
    let cfg = config();
    let at = Timestamp::from_millis(1_700_000_000_000);

    let mut episode_ids = Vec::new();
    for run in ["run-1", "run-2", "run-3"] {
        let id = brain
            .record_trace(
                &space,
                "claude-code",
                run,
                TRACE.into(),
                at,
                TrustTier::Untrusted,
            )
            .await
            .unwrap();
        brain.extract_one(&space, &id, &cfg).await.unwrap();
        episode_ids.push(id);
    }

    let ids = brain.consolidate_runbooks(&space, &cfg, 3).await.unwrap();
    assert_eq!(ids.len(), 1, "one Task anchor → one runbook");
    let rb = &ids[0];

    let db = Connection::open(dir.path().join("brain.db")).unwrap();

    // Derived + terminal: never primary, never extracted.
    let (kind, uncertainty): (String, Option<String>) = db
        .query_row(
            "SELECT kind, uncertainty_json FROM episodes WHERE id = ?1",
            [rb],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "derived");
    assert!(
        uncertainty.as_deref().is_some_and(|u| !u.is_empty()),
        "runbook must carry an Uncertainty block (P10)"
    );

    // P10 sources pairing: linked to every member episode.
    let links: usize = db
        .query_row(
            "SELECT COUNT(*) FROM episode_links WHERE from_episode = ?1 AND rel = 'summarizes'",
            [rb],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(links, 3, "runbook must cite all three source episodes");

    // Content is the cached summary text.
    let content: String = db
        .query_row("SELECT content FROM episodes WHERE id = ?1", [rb], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(content, RUNBOOK);

    // The runbook is excluded from the extraction backlog and has zero
    // assertions — EpisodeKind::Derived is terminal (§5.3).
    let backlog_has_rb: usize = db
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE id = ?1 AND kind = 'primary'",
            [rb],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(backlog_has_rb, 0);
    let assertions_on_rb: usize = db
        .query_row(
            "SELECT COUNT(*) FROM assertions WHERE episode_id = ?1",
            [rb],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(assertions_on_rb, 0, "no assertion may come from a runbook");

    // Replay is a no-op: the checkpoint filter drops the completed cluster.
    let replay = brain.consolidate_runbooks(&space, &cfg, 3).await.unwrap();
    assert!(replay.is_empty(), "replay must not rewrite runbooks");

    // The episode kind is exposed typed, too.
    let episode = brain.get_episode(rb).await.unwrap().unwrap();
    assert_eq!(episode.kind, EpisodeKind::Derived);
}

#[tokio::test]
async fn task_clusters_respect_min_members() {
    let dir = TempDir::new().unwrap();
    let (brain, space) = brain_with_runbook_llm(&dir).await;
    let cfg = config();
    let at = Timestamp::from_millis(1_700_000_000_000);

    for run in ["run-1", "run-2"] {
        let id = brain
            .record_trace(
                &space,
                "claude-code",
                run,
                TRACE.into(),
                at,
                TrustTier::Untrusted,
            )
            .await
            .unwrap();
        brain.extract_one(&space, &id, &cfg).await.unwrap();
    }

    let ids = brain.consolidate_runbooks(&space, &cfg, 3).await.unwrap();
    assert!(
        ids.is_empty(),
        "two members must not meet the default min of 3"
    );
}
