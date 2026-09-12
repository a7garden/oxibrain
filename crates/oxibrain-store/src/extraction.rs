//! Extraction: response cache and claim projection (DESIGN §7).
//!
//! All functions take `&Connection` and are synchronous — they run inside WriteOp
//! transactions on the writer actor, or inside reader pool reads. LLM calls happen
//! OFF these functions, in the Brain facade (§7.2: no LLM inside a transaction).
//!
//! Since schema v11 there is no durable job queue (two-plane design §11.1):
//! the extraction backlog is the `uncached_memory_episodes` query.

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

// ─── Response cache CRUD ─────────────────────────────────────────────────────

/// Cache a raw LLM response for an episode + extractor.
/// INSERT OR REPLACE: re-extraction with the same extractor overwrites.
/// A successful write also consumes the quarantine rows for this
/// (episode, extractor) pair — failures are a retry queue, not an
/// archive; redaction is the only other deleter (§15.5).
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
    // Success consumes the matching quarantine rows: failures are a retry
    // queue, not an archive; redaction is the only other deleter (§15.5).
    conn.execute(
        "DELETE FROM extraction_failures WHERE episode_id = ?1 AND extractor_id = ?2",
        rusqlite::params![episode_id, extractor_id],
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

/// Outcome counts for one extraction run window (the `extract --pending`
/// drain summary). `accepted` counts extraction cache rows written in
/// `[since, until]` — a drained episode that validates and projects writes
/// exactly one; `rejected_episodes` counts distinct episodes with a failure
/// row in the window; `failure_rows` counts those rows (one per
/// repair-exhausted LLM response). Plain counts over a time window:
/// classification and wording live in the caller (P9).
pub struct ExtractionRunCounts {
    pub accepted: u64,
    pub rejected_episodes: u64,
    pub failure_rows: u64,
}

/// Summarize extraction outcomes recorded in `[since, until]`, across all
/// extractors. Read-only; safe next to the writer (WAL).
pub fn extraction_run_counts(
    conn: &Connection,
    since: Timestamp,
    until: Timestamp,
) -> Result<ExtractionRunCounts, BrainError> {
    let count = |sql: &str| -> Result<i64, BrainError> {
        conn.query_row(
            sql,
            rusqlite::params![since.millis(), until.millis()],
            |r| r.get(0),
        )
        .map_err(sql_err)
    };
    Ok(ExtractionRunCounts {
        accepted: count(
            "SELECT COUNT(*) FROM extractions WHERE created_at >= ?1 AND created_at <= ?2",
        )? as u64,
        rejected_episodes: count(
            "SELECT COUNT(DISTINCT episode_id) FROM extraction_failures
             WHERE created_at >= ?1 AND created_at <= ?2",
        )? as u64,
        failure_rows: count(
            "SELECT COUNT(*) FROM extraction_failures WHERE created_at >= ?1 AND created_at <= ?2",
        )? as u64,
    })
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

    // Look up the episode's trust tier once — all assertions from this
    // episode inherit it (P2: trust is a property of the evidence source).
    let trust_str: String = conn
        .query_row(
            "SELECT trust FROM episodes WHERE id = ?1",
            rusqlite::params![episode_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let episode_trust = oxibrain_core::TrustTier::parse_db(&trust_str)
        .unwrap_or(oxibrain_core::TrustTier::Untrusted);

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
            trust: episode_trust,
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
            ..
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

/// Eligible memory-plane backlog (two-plane design §11.1): primary,
/// non-document, not redacted, no extraction row for this extractor, and no
/// extraction failure for this extractor at or after `failure_cutoff`,
/// ordered by seq. This query replaces the retired `ingest_jobs` queue.
///
/// `failure_cutoff` is the retry cooldown: a validation-poison episode
/// (content the extractor can never parse into valid claims) otherwise
/// re-runs its full `max_tokens` generation on every drain — minutes of GPU
/// time per attempt — while the backlog never reaches zero. Pass
/// [`TIME_MAX`] to ignore failures entirely (the explicit `reextract`
/// operator repair path).
pub fn uncached_memory_episodes(
    conn: &Connection,
    space: &str,
    extractor_id: &str,
    failure_cutoff: Timestamp,
) -> Result<Vec<String>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT e.id FROM episodes e
             WHERE e.space_id = ?1 AND e.kind = 'primary'
               AND e.source_kind NOT IN ('document', 'document_revision')
               AND e.redacted_at IS NULL
               AND NOT EXISTS (
                 SELECT 1 FROM extractions x
                 WHERE x.episode_id = e.id AND x.extractor_id = ?2
               )
               AND NOT EXISTS (
                 SELECT 1 FROM extraction_failures f
                 WHERE f.episode_id = e.id AND f.extractor_id = ?2
                   AND f.created_at >= ?3
               )
             ORDER BY e.seq ASC",
        )
        .map_err(sql_err)?;
    let ids: Vec<String> = stmt
        .query_map(
            rusqlite::params![space, extractor_id, failure_cutoff.millis()],
            |r| r.get(0),
        )
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

/// Ensure an episode exists and index it for lexical search.
/// Convenience function for the Brain facade. Queue-less since v11:
/// extraction is driven by `uncached_memory_episodes`, not a job row.
pub fn ingest_episode(
    conn: &Connection,
    space: &str,
    content: &str,
    source: SourceRef,
    trust: oxibrain_core::TrustTier,
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
    Ok(ep_id)
}

/// Event-identity variant of [`ingest_episode`]. Uses `insert_event` with an
/// optional attachment, then indexes for lexical search.
/// `trust` is the server-evaluated trust tier for this episode.
pub fn ingest_event(
    conn: &Connection,
    space: &str,
    content: &str,
    source: SourceRef,
    trust: oxibrain_core::TrustTier,
    attachment: Option<&ledger::IngestAttachment>,
    now: Timestamp,
) -> Result<String, BrainError> {
    let occurred_at = now;
    let mut episode = oxibrain_core::Episode {
        id: String::new(),
        space: space.into(),
        seq: 0,
        content_hash: oxibrain_core::ContentHash([0u8; 32]),
        content: content.into(),
        source,
        trust,
        kind: EpisodeKind::Primary,
        occurred_at,
        ingested_at: now,
        redacted_at: None,
    };
    ledger::insert_event(conn, &mut episode, attachment)?;
    let ep_id = episode.id.clone();

    crate::index_ops::index_episode_fts(conn, &episode.space, &ep_id, &episode.content)?;
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

    /// Insert a test episode with explicit source/kind and return its id.
    fn episode_with(
        conn: &Connection,
        space: &str,
        content: &str,
        source: SourceRef,
        kind: EpisodeKind,
    ) -> String {
        let now = Timestamp::from_millis(2000);
        let mut ep = oxibrain_core::Episode {
            id: String::new(),
            space: space.into(),
            seq: 0,
            content_hash: oxibrain_core::ContentHash([0u8; 32]),
            content: content.into(),
            source,
            trust: oxibrain_core::TrustTier::Trusted,
            kind,
            occurred_at: now,
            ingested_at: now,
            redacted_at: None,
        };
        ledger::insert_episode(conn, &mut ep).unwrap();
        ep.id
    }

    #[test]
    fn uncached_memory_episodes_filters_documents_and_redacted() {
        let (_dir, conn, space) = test_store();
        let now = Timestamp::from_millis(2000);

        // Included: a plain primary note with no extraction row.
        let note = episode_with(
            &conn,
            &space,
            "plain note",
            SourceRef::Note {
                path: "a.md".into(),
            },
            EpisodeKind::Primary,
        );
        // Excluded by source kind: document plane.
        episode_with(
            &conn,
            &space,
            "document content",
            SourceRef::Document {
                uri: "doc://x".into(),
            },
            EpisodeKind::Primary,
        );
        // Excluded by source kind: document revision.
        episode_with(
            &conn,
            &space,
            "document revision content",
            SourceRef::DocumentRevision {
                uri: "doc://x".into(),
            },
            EpisodeKind::Primary,
        );
        // Excluded: redacted.
        let redacted = episode_with(
            &conn,
            &space,
            "redacted note",
            SourceRef::Note {
                path: "b.md".into(),
            },
            EpisodeKind::Primary,
        );
        conn.execute(
            "UPDATE episodes SET redacted_at = 1 WHERE id = ?1",
            rusqlite::params![redacted],
        )
        .unwrap();
        // Excluded: not a primary episode.
        episode_with(
            &conn,
            &space,
            "derived summary",
            SourceRef::Note {
                path: "c.md".into(),
            },
            EpisodeKind::Derived,
        );
        // Excluded for this extractor: already cached by ext1.
        let cached = episode_with(
            &conn,
            &space,
            "cached note",
            SourceRef::Note {
                path: "d.md".into(),
            },
            EpisodeKind::Primary,
        );
        cache_response(&conn, &cached, "ext1", r#"{"claims":[]}"#, now).unwrap();
        // Included: cached only by a different extractor.
        let other_ext = episode_with(
            &conn,
            &space,
            "other extractor note",
            SourceRef::Note {
                path: "e.md".into(),
            },
            EpisodeKind::Primary,
        );
        cache_response(&conn, &other_ext, "ext2", r#"{"claims":[]}"#, now).unwrap();
        // Excluded: different space.
        let space2 = ledger::create_space(&conn, "other_space", now).unwrap();
        episode_with(
            &conn,
            &space2,
            "other space note",
            SourceRef::Note {
                path: "f.md".into(),
            },
            EpisodeKind::Primary,
        );

        let ids = uncached_memory_episodes(&conn, &space, "ext1", TIME_MAX).unwrap();
        assert_eq!(
            ids,
            vec![note.clone(), other_ext],
            "only uncached memory-plane primary episodes, ordered by seq"
        );
    }

    /// A same-extractor failure inside the cooldown hides the episode from
    /// the backlog; an older failure (or [`TIME_MAX`]) does not. This is the
    /// store half of the retry-cooldown contract (2026-09-01 drain incident:
    /// a validation-poison episode re-ran a full generation on every drain).
    #[test]
    fn uncached_memory_episodes_respects_failure_cooldown() {
        let (_dir, conn, space) = test_store();
        let failed_recent = episode_with(
            &conn,
            &space,
            "recent failure",
            SourceRef::Note {
                path: "a.md".into(),
            },
            EpisodeKind::Primary,
        );
        let failed_old = episode_with(
            &conn,
            &space,
            "old failure",
            SourceRef::Note {
                path: "b.md".into(),
            },
            EpisodeKind::Primary,
        );
        crate::quarantine::record_failure(
            &conn,
            &failed_recent,
            "ext1",
            r#"{}"#,
            "[]",
            Timestamp::from_millis(20_000),
        )
        .unwrap();
        crate::quarantine::record_failure(
            &conn,
            &failed_old,
            "ext1",
            r#"{}"#,
            "[]",
            Timestamp::from_millis(5_000),
        )
        .unwrap();

        // Cutoff between the two failures: only the older failure retries.
        let cutoff = Timestamp::from_millis(10_000);
        let ids = uncached_memory_episodes(&conn, &space, "ext1", cutoff).unwrap();
        assert_eq!(ids, vec![failed_old.clone()]);

        // TIME_MAX ignores failures entirely (operator repair path).
        let ids = uncached_memory_episodes(&conn, &space, "ext1", TIME_MAX).unwrap();
        assert_eq!(ids, vec![failed_recent, failed_old]);
    }

    #[test]
    fn cache_roundtrip() {
        let (_dir, conn, space) = test_store();
        let ep = episode_with(
            &conn,
            &space,
            "test content",
            SourceRef::Note {
                path: "g.md".into(),
            },
            EpisodeKind::Primary,
        );
        let now = Timestamp::from_millis(2000);

        cache_response(&conn, &ep, "ext1", r#"{"claims":[]}"#, now).unwrap();

        let cached = get_cached_response(&conn, &ep, "ext1").unwrap();
        assert_eq!(cached.as_deref(), Some(r#"{"claims":[]}"#));

        let missing = get_cached_response(&conn, &ep, "ext2").unwrap();
        assert!(missing.is_none());
    }

    /// Window bucketing for the drain summary: cache rows and failure rows
    /// count only inside `[since, until]`; two failures on one episode fold
    /// into one rejected episode.
    #[test]
    fn extraction_run_counts_bucket_by_window() {
        let (_dir, conn, space) = test_store();
        let in_window = Timestamp::from_millis(20_000);
        let before_window = Timestamp::from_millis(5_000);
        let note = |path: &str, content: &str| {
            episode_with(
                &conn,
                &space,
                content,
                SourceRef::Note { path: path.into() },
                EpisodeKind::Primary,
            )
        };
        let accepted = note("a.md", "accepted note");
        let rejected = note("b.md", "rejected note");
        let stale = note("c.md", "stale note");

        cache_response(&conn, &accepted, "ext1", r#"{"claims":[]}"#, in_window).unwrap();
        crate::quarantine::record_failure(&conn, &rejected, "ext1", r#"{}"#, "[]", in_window)
            .unwrap();
        crate::quarantine::record_failure(&conn, &rejected, "ext1", r#"{}"#, "[]", in_window)
            .unwrap();
        // Outside the window: must not leak into the run's counts.
        cache_response(&conn, &stale, "ext1", r#"{"claims":[]}"#, before_window).unwrap();
        crate::quarantine::record_failure(&conn, &stale, "ext1", r#"{}"#, "[]", before_window)
            .unwrap();

        let counts = extraction_run_counts(
            &conn,
            Timestamp::from_millis(10_000),
            Timestamp::from_millis(30_000),
        )
        .unwrap();
        assert_eq!(counts.accepted, 1);
        assert_eq!(counts.rejected_episodes, 1, "two failures, one episode");
        assert_eq!(counts.failure_rows, 2);
    }
}
