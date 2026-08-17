//! Task 5 (preserve deterministic consolidation during shared use):
//!
//! Focused tests for `ExtractorConfig::provider_profile_id` provenance and
//! the consolidation checkpoint / Uncertainty plumbing. These tests
//! deliberately follow the existing `reproject_determinism` test structure
//! (dump a stable projection before/after a heavy operation, assert
//! byte-identical) so a regression in `extract → consolidate → reproject`
//! determinism is loud.
//!
//! Truth-half vs cache-half invariants enforced here:
//!
//! * The truth-fold (entities, statements, mentions-by-content, beliefs)
//!   is unchanged by which Foundation profile (or none) is bound to the
//!   model — only the cache half (`extractor_id`) changes.
//! * A profile failure before the cache write leaves a resumable
//!   in-progress checkpoint but no derived episode and no mutated source
//!   episode.
//! * After consolidation-backed derived episodes exist, `reproject()`
//!   still produces a byte-identical projection.

use oxibrain::Brain;
use oxibrain_core::extraction::{ExtractMechanism, ExtractorConfig};
use oxibrain_core::{SourceRef, TrustTier};
use oxibrain_ports::{
    BrainError, FakeClock, FakeLlmPort, LlmCapabilities, LlmPort, LlmRequest, LlmResponse,
    Timestamp,
};
use parking_lot::Mutex;
use rusqlite::Connection;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

fn dump_table(conn: &Connection, table: &str, columns: &str, order: &str) -> String {
    let sql = format!("SELECT {columns} FROM {table} ORDER BY {order}");
    let n_cols = columns.split(',').count();
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            let mut parts = Vec::new();
            for i in 0..n_cols {
                let val: String = match r.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Null) => "null".into(),
                    Ok(rusqlite::types::ValueRef::Integer(i)) => i.to_string(),
                    Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                    Ok(rusqlite::types::ValueRef::Text(t)) => {
                        format!("\"{}\"", String::from_utf8_lossy(t))
                    }
                    Ok(rusqlite::types::ValueRef::Blob(b)) => format!("blob({})", b.len()),
                    Err(_) => "?".into(),
                };
                parts.push(val);
            }
            Ok(parts.join(","))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.join(";")
}

/// Dump the truth-half of the store: every column that does NOT carry
/// extraction provenance. `assertions` and the `id` column of `mentions`
/// are derived from `extractor_id` (see `assertion_id` / `mention_id` in
/// `oxibrain-core::id`) and reproject rebuilds them from the
/// `extractions` cache. The durable truth-half is the entity /
/// statement / mention-content / belief / episode graph — every column
/// dumped below survives a reproject unchanged regardless of the
/// Foundation profile bound to the model.
fn dump_truth_fold(conn: &Connection) -> String {
    let mut out = String::new();
    for (table, cols, order) in [
        (
            "entities",
            "id, space_id, type_name, canonical_key, created_at, merged_into",
            "id",
        ),
        (
            "entity_keys",
            "id, space_id, entity_id, type_name, normalized, surface, origin",
            "id",
        ),
        (
            "statements",
            "id, space_id, subject_id, predicate, object_entity, object_literal",
            "id",
        ),
        (
            "mentions",
            "role, surface, span_start, span_end, resolved_to, method",
            "role, surface, span_start, resolved_to",
        ),
        (
            "beliefs",
            "statement_id, valid_from, valid_to, status, confidence, support_json",
            "statement_id, valid_from",
        ),
    ] {
        out.push_str(table);
        out.push(':');
        out.push_str(&dump_table(conn, table, cols, order));
        out.push('\n');
    }
    out
}

fn dump_episodes_and_links(conn: &Connection) -> String {
    let mut out = String::new();
    out.push_str("episodes:");
    out.push_str(&dump_table(
        conn,
        "episodes",
        "id, space_id, kind, source_kind, source_ref, content_hash, trust, uncertainty_json, redacted_at",
        "id",
    ));
    out.push('\n');
    out.push_str("episode_links:");
    out.push_str(&dump_table(
        conn,
        "episode_links",
        "from_episode, to_episode, rel",
        "from_episode, to_episode, rel",
    ));
    out.push('\n');
    out
}

fn canned_extraction() -> &'static str {
    r#"{"claims":[
        {"predicate":"works_on","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"ProjectX","entity_type":"Project","span":[15,23]}},"polarity":"affirm","confidence":0.95},
        {"predicate":"employed_by","subject":{"surface":"Alice","entity_type":"Person","span":[0,5]},"object":{"kind":"entity","mention":{"surface":"Acme Corp","entity_type":"Organization","span":[27,36]}},"polarity":"affirm","confidence":0.9}
    ]}"#
}

fn canned_summary() -> &'static str {
    "Cluster summary: Alice works on ProjectX at Acme Corp."
}

fn base_extractor() -> ExtractorConfig {
    ExtractorConfig {
        model_id: "test-model".into(),
        prompt_version: 1,
        registry_major: 1,
        mechanism: ExtractMechanism::JsonSchema,
        max_tokens: 4096,
        model_digest: None,
        provider_profile_id: None,
    }
}

/// A test-only LLM port that delegates to `FakeLlmPort` for canned
/// responses and lets the test flip a switch so the next
/// consolidation-prompt call errors. Extraction prompts always pass
/// through; the failure mode is keyed off the consolidation prompt's
/// unique header so the two paths are independent.
struct FlippableLlm {
    inner: Arc<FakeLlmPort>,
    fail_consolidation: Arc<AtomicBool>,
    // Track call history for assertions in tests.
    consolidation_calls: Arc<Mutex<usize>>,
}

impl FlippableLlm {
    fn new() -> Self {
        Self {
            inner: Arc::new(FakeLlmPort::new()),
            fail_consolidation: Arc::new(AtomicBool::new(false)),
            consolidation_calls: Arc::new(Mutex::new(0)),
        }
    }
    fn respond_to(&self, key: &str, resp: LlmResponse) {
        self.inner.respond_to(key, resp);
    }
    fn set_fail_consolidation(&self, fail: bool) {
        self.fail_consolidation.store(fail, Ordering::SeqCst);
    }
    fn consolidation_calls(&self) -> usize {
        *self.consolidation_calls.lock()
    }
}

#[async_trait::async_trait]
impl LlmPort for FlippableLlm {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        if req
            .prompt
            .contains("Summarize the following related episodes")
        {
            *self.consolidation_calls.lock() += 1;
            if self.fail_consolidation.load(Ordering::SeqCst) {
                return Err(BrainError::Provider {
                    retryable: true,
                    message: "FlippableLlm: profile-failure simulation".into(),
                });
            }
        }
        self.inner.complete(req).await
    }
    async fn generate_constrained(
        &self,
        req: LlmRequest,
        grammar: &str,
    ) -> Result<LlmResponse, BrainError> {
        self.inner.generate_constrained(req, grammar).await
    }
    fn capabilities(&self) -> LlmCapabilities {
        self.inner.capabilities()
    }
}

/// Ingest + extract two primary episodes that share entities, so
/// `find_episode_clusters` produces a 2-member cluster. Both episodes
/// carry the entity-mention substrings the canned extraction response
/// expects at the exact same byte offsets, so each extraction yields two
/// valid claims and the two episodes share all three entities (Alice,
/// ProjectX, Acme Corp), well above the `find_episode_clusters` ≥ 2
/// shared-statement threshold. The two contents differ (one is the
/// canonical sentence, the other is the canonical sentence with a
/// trailing note) so the `(space, content_hash)` idempotency layer lets
/// both rows land in the ledger.
async fn ingest_pair_with_shared_entities(
    brain: &Brain,
    space: &str,
    config: &ExtractorConfig,
) -> (String, String) {
    let content_a = "Alice works on ProjectX at Acme Corp";
    let content_b = "Alice works on ProjectX at Acme Corp — daily standup notes";

    let ep_a = brain
        .ingest(
            space,
            content_a.into(),
            SourceRef::Note {
                path: "a.md".into(),
            },
            TrustTier::Trusted,
            &config.id(),
        )
        .await
        .unwrap();
    let ep_b = brain
        .ingest(
            space,
            content_b.into(),
            SourceRef::Note {
                path: "b.md".into(),
            },
            TrustTier::Trusted,
            &config.id(),
        )
        .await
        .unwrap();

    brain.extract_one(space, &ep_a, config).await.unwrap();
    brain.extract_one(space, &ep_b, config).await.unwrap();
    (ep_a, ep_b)
}

// ─────────────────────────────────────────────────────────────────────────
// Test 1: profile identity changes cache provenance without changing the
// truth-fold output.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn profile_identity_changes_cache_only_not_truth() {
    let dir = TempDir::new().unwrap();
    let config_a = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());

    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm_a = Arc::new(FakeLlmPort::new());
    llm_a.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_extraction().into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain_a = Brain::with_llm(config_a, clock.clone(), llm_a)
        .await
        .unwrap();
    let space_a = brain_a.ensure_space("profile_test").await.unwrap();

    let cfg_no_profile = base_extractor();
    ingest_pair_with_shared_entities(&brain_a, &space_a, &cfg_no_profile).await;

    let db_path = dir.path().join("brain.db");
    let conn_a = Connection::open(&db_path).unwrap();
    let truth_a = dump_truth_fold(&conn_a);
    drop(conn_a);

    let cfg_with_profile = ExtractorConfig {
        provider_profile_id: Some("default-extract".into()),
        ..cfg_no_profile.clone()
    };

    let dir2 = TempDir::new().unwrap();
    let config_b = oxibrain::BrainConfig::at(dir2.path().to_str().unwrap());
    let llm_b = Arc::new(FakeLlmPort::new());
    llm_b.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_extraction().into(),
            raw: serde_json::Value::Null,
        },
    );
    let brain_b = Brain::with_llm(config_b, clock, llm_b).await.unwrap();
    let space_b = brain_b.ensure_space("profile_test").await.unwrap();
    ingest_pair_with_shared_entities(&brain_b, &space_b, &cfg_with_profile).await;

    let db_path_b = dir2.path().join("brain.db");
    let conn_b = Connection::open(&db_path_b).unwrap();
    let truth_b = dump_truth_fold(&conn_b);
    drop(conn_b);

    assert_eq!(
        truth_a, truth_b,
        "truth-fold must not depend on provider_profile_id"
    );

    let conn_a = Connection::open(db_path).unwrap();
    let conn_b = Connection::open(db_path_b).unwrap();
    let ext_a: String = conn_a
        .query_row(
            "SELECT DISTINCT extractor_id FROM assertions LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let ext_b: String = conn_b
        .query_row(
            "SELECT DISTINCT extractor_id FROM assertions LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn_a);
    drop(conn_b);
    assert_ne!(
        ext_a, ext_b,
        "provider_profile_id must change extractor_id (cache provenance)"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Test 2: a profile failure mid-consolidation leaves a resumable
// in-progress checkpoint and no derived episode.
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn consolidation_failure_leaves_resumable_checkpoint() {
    let dir = TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm = Arc::new(FlippableLlm::new());
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_extraction().into(),
            raw: serde_json::Value::Null,
        },
    );
    llm.respond_to(
        "Summarize the following related episodes",
        LlmResponse {
            text: canned_summary().into(),
            raw: serde_json::Value::Null,
        },
    );
    // Engage the profile-failure switch for the first consolidation call.
    llm.set_fail_consolidation(true);

    let brain = Brain::with_llm(config, clock, llm.clone()).await.unwrap();
    let space = brain.ensure_space("consol_fail").await.unwrap();

    let cfg = ExtractorConfig {
        provider_profile_id: Some("default-consolidate".into()),
        ..base_extractor()
    };
    let (_ep_a, _ep_b) = ingest_pair_with_shared_entities(&brain, &space, &cfg).await;

    // Consolidation must fail (FlippableLlm errors on the first call).
    let err = brain
        .consolidate(&space, &cfg)
        .await
        .expect_err("consolidate must fail when the LLM port returns an error");
    let msg = format!("{err}");
    assert!(
        !msg.contains("channel dropped") && !msg.contains("join:"),
        "profile failure must not be reported as a storage/join error: {msg}"
    );
    assert_eq!(
        llm.consolidation_calls(),
        1,
        "the failing run reaches the LLM exactly once"
    );

    // 1. No derived episode was written.
    let db_path = dir.path().join("brain.db");
    let conn = Connection::open(&db_path).unwrap();
    let derived_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE kind = 'derived'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        derived_count, 0,
        "no derived episode must exist after a failed consolidation"
    );

    // 2. No source episodes were mutated.
    let primary_mismatches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE kind = 'primary' AND content_hash IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        primary_mismatches, 0,
        "source episodes must retain their content_hash"
    );

    // 3. An in-progress checkpoint row exists for the cluster.
    let in_progress: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consolidation_checkpoints WHERE status = 'in_progress'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        in_progress, 1,
        "exactly one in-progress checkpoint must exist for the failed cluster"
    );

    // 4. The cache is empty — no summary was written before the derived episode.
    let cached: i64 = conn
        .query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        cached, 0,
        "no cached summary may be written before the derived episode"
    );

    drop(conn);

    // 5. RESUME: flip the failure switch off so the LLM now serves the
    //    canned summary response. The in-progress checkpoint still marks
    //    the cluster, so the resumed run picks it up and finishes it.
    llm.set_fail_consolidation(false);
    let resumed_ids = brain.consolidate(&space, &cfg).await.unwrap();
    assert_eq!(
        resumed_ids.len(),
        1,
        "the cluster resumes and writes one derived episode"
    );
    assert_eq!(
        llm.consolidation_calls(),
        2,
        "the resumed run reaches the LLM a second time"
    );

    let conn = Connection::open(db_path).unwrap();
    let derived_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE kind = 'derived'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        derived_count, 1,
        "the resumed run writes the derived episode"
    );

    let completed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consolidation_checkpoints WHERE status = 'completed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(completed, 1, "checkpoint transitions to completed");

    let cached: i64 = conn
        .query_row("SELECT COUNT(*) FROM summaries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        cached, 1,
        "the cached summary lands together with the derived episode"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Test 3: full reproject equivalence after consolidation-backed derived
// episodes exist. This is the highest-value determinism pattern in the
// repo (follows `reproject_determinism::reproject_is_byte_identical`).
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn reproject_byte_identical_after_consolidation() {
    let dir = TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(10000)));
    let llm = Arc::new(FlippableLlm::new());
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_extraction().into(),
            raw: serde_json::Value::Null,
        },
    );
    llm.respond_to(
        "Summarize the following related episodes",
        LlmResponse {
            text: canned_summary().into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain = Brain::with_llm(config, clock, llm).await.unwrap();
    let space = brain.ensure_space("reproject").await.unwrap();

    let cfg = base_extractor();
    let _ = ingest_pair_with_shared_entities(&brain, &space, &cfg).await;
    let derived = brain.consolidate(&space, &cfg).await.unwrap();
    assert_eq!(
        derived.len(),
        1,
        "expected one derived episode from the cluster"
    );

    let db_path = dir.path().join("brain.db");
    let conn_before = Connection::open(&db_path).unwrap();
    let mut before = dump_truth_fold(&conn_before);
    before.push_str(&dump_episodes_and_links(&conn_before));
    let derived_count_before: i64 = conn_before
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE kind = 'derived'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn_before);
    assert!(
        !before.is_empty(),
        "projection must be non-empty before reproject"
    );
    assert_eq!(
        derived_count_before, 1,
        "exactly one derived episode pre-reproject"
    );

    brain.reproject().await.unwrap();

    let conn_after = Connection::open(&db_path).unwrap();
    let mut after = dump_truth_fold(&conn_after);
    after.push_str(&dump_episodes_and_links(&conn_after));
    let derived_count_after: i64 = conn_after
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE kind = 'derived'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn_after);

    assert_eq!(
        derived_count_before, derived_count_after,
        "derived episode count must survive reproject (it is a ledger row)"
    );
    assert_eq!(
        before, after,
        "projection + derived episodes + links must be byte-identical after reproject"
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Test 4: summarize_communities_impl attaches sources + Uncertainty +
// transitions checkpoint in_progress → completed for every cached
// community. Without a community detection function, seed the
// `communities` table directly so the test exercises exactly the path
// that previously wrote derived episodes with `sources: &[]` and
// `uncertainty: None` (Task 5 Finding 3).
// ─────────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn summarize_communities_attaches_sources_and_uncertainty() {
    let dir = TempDir::new().unwrap();
    let brain_cfg = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(20000)));

    // Distinct canned extraction per-call so the FakeLlmPort dispatches
    // by prompt-substring to the right canned response. Extraction uses
    // the canned extraction JSON; community summarisation uses the
    // canned summary text.
    let llm: Arc<FakeLlmPort> = Arc::new(FakeLlmPort::new());
    llm.respond_to(
        "Alice works on",
        LlmResponse {
            text: canned_extraction().into(),
            raw: serde_json::Value::Null,
        },
    );
    llm.respond_to(
        "Summarize the key themes",
        LlmResponse {
            text: "Community summary: Alice and her projects at Acme Corp.".into(),
            raw: serde_json::Value::Null,
        },
    );

    let brain = Brain::with_llm(brain_cfg, clock.clone(), llm)
        .await
        .unwrap();
    let space = brain.ensure_space("community_test").await.unwrap();
    let cfg = base_extractor();
    let (ep_a, ep_b) = ingest_pair_with_shared_entities(&brain, &space, &cfg).await;

    // Seed a community grouping whatever entities the two extractions
    // produced (their canonical keys — alice/projectx/acme — are
    // resolved into entity rows via the registry; we read them back
    // rather than guess at deterministic id strings).
    let db_path = dir.path().join("brain.db");
    let entity_ids: Vec<String> = {
        let conn = Connection::open(&db_path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id FROM entities WHERE space_id = ?1 AND merged_into IS NULL
                 ORDER BY id ASC LIMIT 3",
            )
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([&space], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(
            rows.len(),
            3,
            "expected 3 entities (alice, projectx, acme), got {rows:?}"
        );
        rows
    };
    {
        let conn = Connection::open(&db_path).unwrap();
        for eid in entity_ids.iter() {
            conn.execute(
                "INSERT INTO communities (space_id, label, id) VALUES (?1, ?2, ?3)",
                rusqlite::params![&space, 0i64, eid],
            )
            .unwrap();
        }
    }

    // Pre-flight: confirm extractor_id rows for the community
    // checkpoint are absent so we can prove the transition below.
    {
        let conn = Connection::open(&db_path).unwrap();
        let in_progress: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM consolidation_checkpoints
                 WHERE extractor_id = ?1",
                [&cfg.id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_progress, 0, "no checkpoint rows pre-summarise");
        let derived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE kind = 'derived'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(derived, 0, "no derived episodes pre-summarise");
    }

    // Act.
    let n = brain.summarize_communities(&space, &cfg).await.unwrap();
    assert_eq!(
        n, 1,
        "exactly one community group, so exactly one cache row + derived episode"
    );

    // Post-conditions:
    //   1. community summaries cache row exists for the
    //      namespace-shifted entity-set hash (scope_kind = 'community').
    //   2. derived episode row exists with kind='derived' and
    //      of-source pointing at the two primary episodes.
    //   3. checkpoint transitions to status='completed' (not stuck in
    //      'in_progress').
    //   4. episode_links rows tie the derived episode to its sources.
    //   5. uncertainty_json is populated (non-null) — the brief
    //      forbids writing a derived episode with `None`.
    let conn = Connection::open(&db_path).unwrap();

    let summary_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM summaries
             WHERE scope_kind = 'community' AND extractor_id = ?1",
            [&cfg.id()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(summary_rows, 1, "exactly one cached community summary");

    let derived: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE kind = 'derived'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(derived, 1, "exactly one derived episode row");

    // The derived episode's source ref must point at both source primary
    // episodes (the JSON array of source ids lives in `source_ref`).
    let (source_ref, uncertainty_json): (String, Option<String>) = conn
        .query_row(
            "SELECT source_ref, uncertainty_json FROM episodes WHERE kind = 'derived'",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap();
    assert!(
        source_ref.contains(&ep_a) && source_ref.contains(&ep_b),
        "derived source_ref must cite both primary episodes; got {source_ref}"
    );
    assert!(
        uncertainty_json.is_some(),
        "derived episode must carry uncertainty_json (Task 5 Finding 3)"
    );

    // Checkpoint completed (not stuck in_progress).
    let stuck: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consolidation_checkpoints
             WHERE status = 'in_progress'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stuck, 0, "no stranded in_progress checkpoints");
    let completed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consolidation_checkpoints
             WHERE status = 'completed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(completed, 1, "exactly one completed community checkpoint");

    // No episode_links are written by summarize_communities today, but
    // any links written must point to the two source primary episodes.
    let link_targets: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT to_episode FROM episode_links WHERE rel = 'summarizes'")
            .unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    for link in &link_targets {
        assert!(
            link == &ep_a || link == &ep_b,
            "episode_links.summarizes must point at the source primary episodes; got {link}"
        );
    }
}
