//! Extraction: job queue lifecycle, response cache, and claim projection (DESIGN §7).
//!
//! All functions take `&Connection` and are synchronous — they run inside WriteOp
//! transactions on the writer actor, or inside reader pool reads. LLM calls happen
//! OFF these functions, in the Brain facade (§7.2: no LLM inside a transaction).

use crate::knowledge as kcrud;
use crate::ledger;
use crate::project::{EntityRef, ResolutionCache, resolve_or_create};
use crate::registry;
use crate::sql_err;
use oxibrain_core::confidence::CalibrationTable;
use oxibrain_core::extraction::{Claim, ClaimObject, ExtractionResponse};
use oxibrain_core::fold::fold;
use oxibrain_core::id::{assertion_id, mention_id, statement_id};
use oxibrain_core::knowledge::{
    Assertion, Mention, MentionRole, Object, ResolutionMethod, Statement, TypedValue,
};
use oxibrain_core::{EpisodeKind, SourceRef};
use oxibrain_ports::{BrainError, TIME_MAX, TIME_MIN, Timestamp};
use rusqlite::Connection;

// ─── Job queue types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct IngestJob {
    pub id: String,
    pub episode_id: String,
    pub extractor_id: String,
    pub state: JobState,
    pub session_hint: Option<String>,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub lease_until: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Ready,
    Leased,
    Done,
    Failed,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Leased => "leased",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ready" => Some(Self::Ready),
            "leased" => Some(Self::Leased),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

// ─── Job queue CRUD ──────────────────────────────────────────────────────────

/// Enqueue an extraction job for an episode. Idempotent: same (episode, extractor)
/// → same job id (INSERT OR IGNORE).
pub fn enqueue_job(
    conn: &Connection,
    episode_id: &str,
    extractor_id: &str,
    now: Timestamp,
) -> Result<String, BrainError> {
    let job_id = job_id(episode_id, extractor_id);
    conn.execute(
        "INSERT OR IGNORE INTO ingest_jobs (id, episode_id, extractor_id, state, attempts, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'ready', 0, ?4, ?4)",
        rusqlite::params![job_id, episode_id, extractor_id, now.millis()],
    )
    .map_err(sql_err)?;
    Ok(job_id)
}

/// Claim up to `limit` ready jobs for an extractor. Sets state=leased.
pub fn claim_jobs(
    conn: &Connection,
    extractor_id: &str,
    lease_timeout_secs: u64,
    limit: usize,
    now: Timestamp,
) -> Result<Vec<IngestJob>, BrainError> {
    let lease_until = Timestamp::from_millis(now.millis() + (lease_timeout_secs as i64 * 1000));

    // Atomically claim: UPDATE then SELECT.
    conn.execute(
        "UPDATE ingest_jobs SET state = 'leased', lease_until = ?1, updated_at = ?2
         WHERE id IN (
           SELECT id FROM ingest_jobs
           WHERE state = 'ready' AND extractor_id = ?3
           ORDER BY created_at ASC LIMIT ?4
         )",
        rusqlite::params![
            lease_until.millis(),
            now.millis(),
            extractor_id,
            limit as i64
        ],
    )
    .map_err(sql_err)?;

    let mut stmt = conn
        .prepare(
            "SELECT id, episode_id, extractor_id, state, session_hint, attempts,
                    last_error, lease_until, created_at, updated_at
             FROM ingest_jobs
             WHERE state = 'leased' AND extractor_id = ?1 AND lease_until = ?2",
        )
        .map_err(sql_err)?;

    let jobs = stmt
        .query_map(rusqlite::params![extractor_id, lease_until.millis()], |r| {
            Ok(IngestJob {
                id: r.get(0)?,
                episode_id: r.get(1)?,
                extractor_id: r.get(2)?,
                state: JobState::parse(&r.get::<_, String>(3)?).unwrap_or(JobState::Failed),
                session_hint: r.get(4)?,
                attempts: r.get::<_, i64>(5)? as u32,
                last_error: r.get(6)?,
                lease_until: r.get::<_, Option<i64>>(7)?.map(Timestamp::from_millis),
                created_at: Timestamp::from_millis(r.get(8)?),
                updated_at: Timestamp::from_millis(r.get(9)?),
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;

    Ok(jobs)
}

/// Complete a job: state=done.
pub fn complete_job(conn: &Connection, job_id: &str, now: Timestamp) -> Result<(), BrainError> {
    conn.execute(
        "UPDATE ingest_jobs SET state = 'done', updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now.millis(), job_id],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Fail a job: increment attempts. If attempts >= max, state=failed; else state=ready.
/// Returns the resulting state.
pub fn fail_job(
    conn: &Connection,
    job_id: &str,
    error: &str,
    max_attempts: u32,
    now: Timestamp,
) -> Result<JobState, BrainError> {
    // Read current attempts.
    let attempts: i64 = conn
        .query_row(
            "SELECT attempts FROM ingest_jobs WHERE id = ?1",
            rusqlite::params![job_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;

    let new_attempts = attempts + 1;
    let new_state = if new_attempts as u32 >= max_attempts {
        JobState::Failed
    } else {
        JobState::Ready
    };

    conn.execute(
        "UPDATE ingest_jobs SET attempts = ?1, state = ?2, last_error = ?3, lease_until = NULL, updated_at = ?4
         WHERE id = ?5",
        rusqlite::params![new_attempts, new_state.as_str(), error, now.millis(), job_id],
    )
    .map_err(sql_err)?;

    Ok(new_state)
}

/// Reclaim expired leases: state=leased AND lease_until < now → state=ready.
pub fn reclaim_expired(conn: &Connection, now: Timestamp) -> Result<usize, BrainError> {
    let count = conn
        .execute(
            "UPDATE ingest_jobs SET state = 'ready', lease_until = NULL, updated_at = ?1
         WHERE state = 'leased' AND lease_until < ?2",
            rusqlite::params![now.millis(), now.millis()],
        )
        .map_err(sql_err)?;
    Ok(count)
}

/// List jobs, optionally filtered by state.
pub fn list_jobs(conn: &Connection, state: Option<JobState>) -> Result<Vec<IngestJob>, BrainError> {
    let mut sql = String::from(
        "SELECT id, episode_id, extractor_id, state, session_hint, attempts,
                last_error, lease_until, created_at, updated_at
         FROM ingest_jobs",
    );
    if state.is_some() {
        sql.push_str(" WHERE state = ?1");
    }
    sql.push_str(" ORDER BY created_at ASC");

    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let map_fn = |r: &rusqlite::Row| {
        Ok(IngestJob {
            id: r.get(0)?,
            episode_id: r.get(1)?,
            extractor_id: r.get(2)?,
            state: JobState::parse(&r.get::<_, String>(3)?).unwrap_or(JobState::Failed),
            session_hint: r.get(4)?,
            attempts: r.get::<_, i64>(5)? as u32,
            last_error: r.get(6)?,
            lease_until: r.get::<_, Option<i64>>(7)?.map(Timestamp::from_millis),
            created_at: Timestamp::from_millis(r.get(8)?),
            updated_at: Timestamp::from_millis(r.get(9)?),
        })
    };

    let jobs = if let Some(s) = state {
        stmt.query_map(rusqlite::params![s.as_str()], map_fn)
    } else {
        stmt.query_map([], map_fn)
    }
    .map_err(sql_err)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(sql_err)?;

    Ok(jobs)
}

fn job_id(episode_id: &str, extractor_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(episode_id.as_bytes());
    hasher.update(extractor_id.as_bytes());
    hex::encode(hasher.finalize().as_bytes())
}

// ─── Response cache CRUD ─────────────────────────────────────────────────────

/// Cache a raw LLM response for an episode + extractor.
/// INSERT OR REPLACE: re-extraction with the same extractor overwrites.
pub fn cache_response(
    conn: &Connection,
    episode_id: &str,
    extractor_id: &str,
    raw_response: &str,
    now: Timestamp,
) -> Result<(), BrainError> {
    let hash = oxibrain_core::content_hash(raw_response);
    conn.execute(
        "INSERT OR REPLACE INTO extractions (episode_id, extractor_id, response_hash, raw_response, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![episode_id, extractor_id, hash.0.as_slice(), raw_response, now.millis()],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Get a cached response (for reproject / re-extraction check).
pub fn get_cached_response(
    conn: &Connection,
    episode_id: &str,
    extractor_id: &str,
) -> Result<Option<String>, BrainError> {
    let result: Option<String> = conn
        .query_row(
            "SELECT raw_response FROM extractions WHERE episode_id = ?1 AND extractor_id = ?2",
            rusqlite::params![episode_id, extractor_id],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            _ => Err(e),
        })
        .map_err(sql_err)?;
    Ok(result)
}

// ─── project_extraction: claims → assertions ─────────────────────────────────

/// Project valid claims from an extraction into assertions + mentions.
/// Runs inside a WriteOp transaction. Idempotent (content-derived IDs).
#[allow(clippy::too_many_arguments)]
pub fn project_extraction(
    conn: &Connection,
    space: &str,
    episode_id: &str,
    extractor_id: &str,
    claims: &[Claim],
    now: Timestamp,
    cache: &mut ResolutionCache,
) -> Result<usize, BrainError> {
    let mut count = 0;

    for claim in claims {
        // Resolve subject entity.
        let subj_eref = EntityRef {
            surface: claim.subject.surface.clone(),
            ty: claim.subject.entity_type.clone(),
        };
        let (subj_id, subj_method) = resolve_or_create(
            conn,
            space,
            &subj_eref,
            episode_id,
            claim.subject.span.0,
            now,
            &[],
            cache,
        )?;

        // Resolve object.
        let resolved_obj = resolve_claim_object(conn, space, claim, episode_id, now, cache)?;
        let object = resolved_obj.object;
        let obj_mention_data = resolved_obj.entity;

        // Create statement (idempotent).
        let stmt_id = statement_id(space, &subj_id, &claim.predicate, &object);
        let stmt = Statement {
            id: stmt_id.clone(),
            space: space.into(),
            subject: subj_id.clone(),
            predicate: claim.predicate.clone(),
            object: object.clone(),
        };
        kcrud::insert_statement(conn, &stmt)?;

        // Map valid_from/to (None → sentinels).
        let claimed_from = claim
            .valid_from
            .map(Timestamp::from_millis)
            .unwrap_or(TIME_MIN);
        let claimed_to = claim
            .valid_to
            .map(Timestamp::from_millis)
            .unwrap_or(TIME_MAX);

        // Create assertion (idempotent).
        let aid = assertion_id(
            &stmt_id,
            episode_id,
            extractor_id,
            claim.polarity,
            claimed_from,
            claimed_to,
            claim.confidence,
        );
        let assertion = Assertion {
            id: aid.clone(),
            statement: stmt_id.clone(),
            episode: episode_id.to_string(),
            extractor: Some(extractor_id.to_string()),
            polarity: claim.polarity,
            claimed_from,
            claimed_to,
            confidence: claim.confidence,
            recorded_at: now,
            retracted_at: None,
        };
        kcrud::insert_assertion(conn, &assertion)?;

        // Capture subject mention (with real byte span).
        let subj_mention = Mention {
            id: mention_id(&aid, "subject", claim.subject.span.0),
            assertion: aid.clone(),
            role: MentionRole::Subject,
            surface: claim.subject.surface.clone(),
            span: claim.subject.span,
            resolved_to: Some(subj_id.clone()),
            method: subj_method,
        };
        kcrud::insert_mention(conn, &subj_mention)?;

        // Capture object mention (entity objects only).
        if let Some((obj_entity_id, obj_method, obj_surface, obj_span)) = obj_mention_data {
            let obj_mention = Mention {
                id: mention_id(&aid, "object", obj_span.0),
                assertion: aid.clone(),
                role: MentionRole::Object,
                surface: obj_surface,
                span: obj_span,
                resolved_to: Some(obj_entity_id),
                method: obj_method,
            };
            kcrud::insert_mention(conn, &obj_mention)?;
        }

        // Re-fold the affected group.
        let calibration = CalibrationTable::default();
        if let Some(pred_def) = registry::load_predicate(conn, &claim.predicate)? {
            let group = kcrud::get_statement_group(conn, space, &subj_id, &claim.predicate)?;
            let beliefs = fold(&pred_def, &group, now, &calibration);
            let group_stmt_ids: Vec<String> =
                group.iter().map(|e| e.statement.id.clone()).collect();
            kcrud::replace_beliefs(conn, &group_stmt_ids, &beliefs)?;
        }

        count += 1;
    }

    Ok(count)
}

/// Result of resolving a claim's object.
struct ResolvedClaimObject {
    object: Object,
    /// Entity mention data (entity_id, method, surface, span) — None for literals.
    entity: Option<(String, ResolutionMethod, String, (u32, u32))>,
}

/// Resolve a claim object to an `Object` + optional mention data.
fn resolve_claim_object(
    conn: &Connection,
    space: &str,
    claim: &Claim,
    episode_id: &str,
    now: Timestamp,
    cache: &mut ResolutionCache,
) -> Result<ResolvedClaimObject, BrainError> {
    match &claim.object {
        ClaimObject::Entity { mention } => {
            let eref = EntityRef {
                surface: mention.surface.clone(),
                ty: mention.entity_type.clone(),
            };
            let (eid, method) = resolve_or_create(
                conn,
                space,
                &eref,
                episode_id,
                mention.span.0,
                now,
                &[],
                cache,
            )?;
            Ok(ResolvedClaimObject {
                object: Object::Entity(eid.clone()),
                entity: Some((eid, method, mention.surface.clone(), mention.span)),
            })
        }
        ClaimObject::Literal {
            literal_type,
            value,
            span: _,
        } => {
            let tv = parse_claim_literal(literal_type, value)?;
            Ok(ResolvedClaimObject {
                object: Object::Literal(tv),
                entity: None,
            })
        }
    }
}

fn parse_claim_literal(lt: &str, value: &str) -> Result<TypedValue, BrainError> {
    match lt {
        "text" => Ok(TypedValue::Text(value.into())),
        "date" => Ok(TypedValue::Date(value.into())),
        "datetime" => Ok(TypedValue::DateTime(value.into())),
        "number" => {
            let n: f64 = value
                .parse()
                .map_err(|e| BrainError::Invalid(format!("number literal: {e}")))?;
            Ok(TypedValue::Number(n))
        }
        "bool" => {
            let b: bool = value
                .parse()
                .map_err(|e| BrainError::Invalid(format!("bool literal: {e}")))?;
            Ok(TypedValue::Bool(b))
        }
        _ => Ok(TypedValue::Text(value.into())), // enum values treated as text
    }
}

/// Find primary episodes that don't have a cache entry for this extractor.
pub fn uncached_episodes(
    conn: &Connection,
    space: &str,
    extractor_id: &str,
) -> Result<Vec<String>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT e.id FROM episodes e
             WHERE e.space_id = ?1 AND e.kind = 'primary'
             AND NOT EXISTS (
               SELECT 1 FROM extractions x
               WHERE x.episode_id = e.id AND x.extractor_id = ?2
             )
             ORDER BY e.seq ASC",
        )
        .map_err(sql_err)?;
    let ids: Vec<String> = stmt
        .query_map(rusqlite::params![space, extractor_id], |r| r.get(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    Ok(ids)
}

/// Parse + validate + project from a cached response — no LLM call.
#[allow(clippy::too_many_arguments)]
pub fn project_from_cache(
    conn: &Connection,
    space: &str,
    episode_id: &str,
    extractor_id: &str,
    raw_response: &str,
    content: &str,
    now: Timestamp,
    cache: &mut ResolutionCache,
) -> Result<usize, BrainError> {
    let response: ExtractionResponse = serde_json::from_str(raw_response)
        .map_err(|e| BrainError::Extraction(format!("parse cached response: {e}")))?;
    let predicates = oxibrain_core::registry::core_v1();
    let result = oxibrain_core::extraction::validate_claims(&response.claims, content, predicates);
    project_extraction(
        conn,
        space,
        episode_id,
        extractor_id,
        &result.valid,
        now,
        cache,
    )
}

/// Ensure an episode exists and enqueue an extraction job for it.
/// Convenience function for the Brain facade.
pub fn ingest_and_enqueue(
    conn: &Connection,
    space: &str,
    content: &str,
    source: SourceRef,
    trust: oxibrain_core::TrustTier,
    extractor_id: &str,
    now: Timestamp,
) -> Result<String, BrainError> {
    let ch = oxibrain_core::content_hash(content);
    let occurred_at = now;
    let ep_id = oxibrain_core::episode_id(space, &ch, &source, occurred_at);

    let mut episode = oxibrain_core::Episode {
        id: ep_id.clone(),
        space: space.into(),
        seq: 0,
        content_hash: ch,
        content: content.into(),
        source,
        trust,
        kind: EpisodeKind::Primary,
        occurred_at,
        ingested_at: now,
        redacted_at: None,
    };
    ledger::insert_episode(conn, &mut episode)?;
    let ep_id = episode.id.clone();

    crate::index_ops::index_episode_fts(conn, &episode.space, &ep_id, &episode.content)?;

    enqueue_job(conn, &ep_id, extractor_id, now)?;
    Ok(ep_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, Connection, String) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(&dir.path().join("test.db")).unwrap();
        let conn = store.into_parts().0;
        let space_id =
            ledger::create_space(&conn, "test_space", Timestamp::from_millis(1000)).unwrap();
        (dir, conn, space_id)
    }

    /// Insert a test episode and return its id.
    fn test_episode(conn: &Connection, space: &str, content: &str) -> String {
        let now = Timestamp::from_millis(2000);
        let mut ep = oxibrain_core::Episode {
            id: String::new(),
            space: space.into(),
            seq: 0,
            content_hash: oxibrain_core::ContentHash([0u8; 32]),
            content: content.into(),
            source: SourceRef::Note {
                path: "test.md".into(),
            },
            trust: oxibrain_core::TrustTier::Trusted,
            kind: EpisodeKind::Primary,
            occurred_at: now,
            ingested_at: now,
            redacted_at: None,
        };
        ledger::insert_episode(conn, &mut ep).unwrap();
        ep.id
    }

    #[test]
    fn job_lifecycle() {
        let (_dir, conn, space) = test_store();
        let now = Timestamp::from_millis(2000);
        let ep = test_episode(&conn, &space, "test content");

        let job_id = enqueue_job(&conn, &ep, "ext1", now).unwrap();
        assert!(!job_id.is_empty());

        let jobs = claim_jobs(&conn, "ext1", 300, 10, now).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Leased);

        complete_job(&conn, &job_id, now).unwrap();
        let jobs = list_jobs(&conn, Some(JobState::Done)).unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn job_fail_and_retry() {
        let (_dir, conn, space) = test_store();
        let now = Timestamp::from_millis(2000);
        let ep = test_episode(&conn, &space, "test content");

        let job_id = enqueue_job(&conn, &ep, "ext1", now).unwrap();
        claim_jobs(&conn, "ext1", 300, 10, now).unwrap();

        let state = fail_job(&conn, &job_id, "timeout", 3, now).unwrap();
        assert_eq!(state, JobState::Ready);

        let jobs = claim_jobs(&conn, "ext1", 300, 10, now).unwrap();
        assert_eq!(jobs.len(), 1);

        let state = fail_job(&conn, &job_id, "timeout", 3, now).unwrap();
        assert_eq!(state, JobState::Ready);

        claim_jobs(&conn, "ext1", 300, 10, now).unwrap();
        let state = fail_job(&conn, &job_id, "timeout", 3, now).unwrap();
        assert_eq!(state, JobState::Failed);
    }

    #[test]
    fn reclaim_expired_leases() {
        let (_dir, conn, space) = test_store();
        let now = Timestamp::from_millis(2000);
        let ep = test_episode(&conn, &space, "test content");

        enqueue_job(&conn, &ep, "ext1", now).unwrap();
        claim_jobs(&conn, "ext1", 10, 10, now).unwrap();

        let later = Timestamp::from_millis(20000);
        let reclaimed = reclaim_expired(&conn, later).unwrap();
        assert_eq!(reclaimed, 1);

        let jobs = claim_jobs(&conn, "ext1", 300, 10, later).unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn enqueue_is_idempotent() {
        let (_dir, conn, space) = test_store();
        let now = Timestamp::from_millis(2000);
        let ep = test_episode(&conn, &space, "test content");

        let id1 = enqueue_job(&conn, &ep, "ext1", now).unwrap();
        let id2 = enqueue_job(&conn, &ep, "ext1", now).unwrap();
        assert_eq!(id1, id2);

        let jobs = list_jobs(&conn, None).unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn cache_roundtrip() {
        let (_dir, conn, space) = test_store();
        let now = Timestamp::from_millis(2000);
        let ep = test_episode(&conn, &space, "test content");

        cache_response(&conn, &ep, "ext1", r#"{"claims":[]}"#, now).unwrap();

        let cached = get_cached_response(&conn, &ep, "ext1").unwrap();
        assert_eq!(cached.as_deref(), Some(r#"{"claims":[]}"#));

        let missing = get_cached_response(&conn, &ep, "ext2").unwrap();
        assert!(missing.is_none());
    }
}
