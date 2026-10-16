//! Reprojection: drop all projection tables and replay the ledger (DESIGN §14.3).
//! The single most valuable test in the suite — proves P1 (byte-identical rebuild).

use crate::project::{ResolutionCache, parse_declaration, project_declaration};
use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;
use serde::Serialize;

/// Outcome summary of a full reprojection replay.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ReprojectReport {
    /// Declaration episodes successfully replayed into the projection.
    pub declarations_replayed: usize,
    /// Declaration episodes that failed to parse and were skipped (counted,
    /// logged, never mutated — the ledger is immutable).
    pub declarations_skipped: usize,
}

/// Replay declarations and cached extractions in their recorded transaction
/// timeline, with source sequence and extractor identity as deterministic ties.
/// The semantic projection must match the incremental store.
pub fn reproject(conn: &Connection) -> Result<ReprojectReport, BrainError> {
    // 1. Delete all projection rows (order respects FK constraints).
    // Beliefs first (FK to statements), then mentions (FK to assertions),
    // then assertions (FK to statements), then statements,
    // then entity_merges, entity_keys, entities.
    for table in [
        "beliefs",
        "mentions",
        "assertions",
        "statements",
        "entity_merges",
        "entity_keys",
        "entities",
    ] {
        conn.execute(&format!("DELETE FROM {table}"), [])
            .map_err(sql_err)?;
    }

    // 2. Read all Declaration episodes in seq order, with their ingested_at.
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, content, ingested_at, seq
             FROM episodes
            WHERE kind = 'declaration' AND redacted_at IS NULL
            ORDER BY seq ASC",
        )
        .map_err(sql_err)?;

    let episodes: Vec<(String, String, String, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, // id
                r.get::<_, String>(1)?, // space_id
                r.get::<_, String>(2)?, // content
                r.get::<_, i64>(3)?,    // ingested_at
                r.get::<_, i64>(4)?,    // seq
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt); // release the prepared statement before we write

    // 3. Replay in true event order. The incremental path projects in
    //    wall-clock order: an extraction projects when the extractor ran
    //    (`extractions.created_at`), a declaration when it was declared
    //    (`episodes.ingested_at`). Entity ids derive from the first event
    //    that creates the entity (`entity_id(space, ty, episode_id,
    //    span_start)`), and belief folds cut assertion visibility at the
    //    event's transaction time — so replaying the two phases separately
    //    (all declarations, then all extractions) re-derives entity ids from
    //    the wrong episode and hides later assertions from earlier folds.
    //    Interleaving by event time reproduces the incremental timeline;
    //    the (seq, kind, extractor_id) tail keeps the order total and
    //    deterministic when events share a millisecond (declarations first).
    //
    //    Declarations replay with their ORIGINAL ingested_at; extraction
    //    replays with the cache's ORIGINAL created_at. This reproduces the
    //    exact occurred_at/recorded_at/episode ids from the incremental
    //    path — required for byte-identical output. Both projections are
    //    idempotent (INSERT OR IGNORE), so re-inserting rows is a no-op.
    //
    //    Shared `ResolutionCache` so the LSH blocking index is built once per
    //    (space, type) for the whole reproject (M9 §10.1 — sublinear per
    //    mention amortized, not just per individual resolution).
    enum ReplayEvent {
        Declaration {
            ep_id: String,
            space: String,
            content: String,
            ingested_at: i64,
            seq: i64,
        },
        Extraction {
            ep_id: String,
            space: String,
            content: String,
            created_at: i64,
            seq: i64,
            extractor_id: String,
            raw: String,
        },
    }
    impl ReplayEvent {
        /// Total deterministic order: (event time, source seq, declarations
        /// before extractions within the same millisecond, extractor id).
        fn key(&self) -> (i64, i64, u8, &str) {
            match self {
                ReplayEvent::Declaration { ingested_at, seq, .. } => {
                    (*ingested_at, *seq, 0, "")
                }
                ReplayEvent::Extraction { created_at, seq, extractor_id, .. } => {
                    (*created_at, *seq, 1, extractor_id)
                }
            }
        }
    }

    let mut events: Vec<ReplayEvent> = Vec::with_capacity(episodes.len());
    for (ep_id, space, content, ingested_at, seq) in episodes {
        events.push(ReplayEvent::Declaration { ep_id, space, content, ingested_at, seq });
    }

    // 3.5. Collect cached extractions (deterministic — no LLM call). Each
    //      replays at its original extraction time, positioned among the
    //      declarations by the merged sort below.
    let mut ext_stmt = conn
        .prepare(
            "SELECT e.id, e.space_id, e.content, x.created_at, e.seq,
                    x.extractor_id, x.raw_response
             FROM extractions x
             JOIN episodes e ON x.episode_id = e.id
            WHERE e.kind = 'primary' AND e.redacted_at IS NULL",
        )
        .map_err(sql_err)?;

    let extractions: Vec<(String, String, String, i64, i64, String, String)> = ext_stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, // episode_id
                r.get::<_, String>(1)?, // space_id
                r.get::<_, String>(2)?, // content
                r.get::<_, i64>(3)?,    // extraction created_at
                r.get::<_, i64>(4)?,    // episode seq
                r.get::<_, String>(5)?, // extractor_id
                r.get::<_, String>(6)?, // raw_response
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(ext_stmt);

    events.reserve(extractions.len());
    for (ep_id, space, content, created_at, seq, extractor_id, raw) in extractions {
        events.push(ReplayEvent::Extraction { ep_id, space, content, created_at, seq, extractor_id, raw });
    }

    events.sort_unstable_by(|a, b| a.key().cmp(&b.key()));

    let mut cache = ResolutionCache::new();
    let mut report = ReprojectReport::default();
    for event in &events {
        match event {
            ReplayEvent::Declaration { ep_id, space, content, ingested_at, .. } => {
                let decl = match parse_declaration(content) {
                    Ok(decl) => decl,
                    Err(e) => {
                        // One unparseable episode must never abort the replay.
                        // Count, log, skip — the episode itself stays untouched
                        // in the ledger.
                        tracing::warn!(
                            episode_id = %ep_id,
                            error = %e,
                            "reproject: skipping unparseable declaration episode"
                        );
                        report.declarations_skipped += 1;
                        continue;
                    }
                };
                project_declaration(conn, space, &decl, Timestamp(*ingested_at), &mut cache)?;
                report.declarations_replayed += 1;
            }
            ReplayEvent::Extraction { ep_id, space, content, created_at, extractor_id, raw, .. } => {
                crate::extraction::project_from_cache(
                    conn,
                    space,
                    ep_id,
                    extractor_id,
                    raw,
                    content,
                    Timestamp(*created_at),
                    &mut cache,
                )?;
            }
        }
    }
    drop(events);

    // 3.6. Replay redactions (entity-scoped). After extraction replay, deleted
    //      assertions have been re-created from cache. Redactions recorded in
    //      the `redactions` table delete them again, so the projection matches
    //      the incremental path. Episode-scoped redaction is already handled
    //      by the `redacted_at IS NULL` filters above.
    let redactions = crate::security::list_redactions(conn)?;
    for (target_json, _reason, redacted_at) in &redactions {
        if let Ok(target) =
            serde_json::from_str::<oxibrain_core::security::RedactTarget>(target_json)
        {
            crate::redaction::apply_replay(conn, &target, *redacted_at)?;
        }
    }

    // 4. Rebuild indexes (FTS5, TF-IDF, salience) and communities for every
    //    space that survived replay. Index tables are derived state, so they
    //    must be rebuilt to match the freshly replayed projection — and doing
    //    so here makes reproject the canonical source of truth.
    let mut space_stmt = conn
        .prepare("SELECT DISTINCT id FROM spaces")
        .map_err(sql_err)?;
    let spaces: Vec<String> = space_stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(space_stmt);
    for space in &spaces {
        crate::index_ops::rebuild_indexes(conn, space)?;
        crate::communities::rebuild_communities(conn, space)?;
    }

    Ok(report)
}
