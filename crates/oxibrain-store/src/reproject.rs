//! Reprojection: drop all projection tables and replay the ledger (DESIGN §14.3).
//! The single most valuable test in the suite — proves P1 (byte-identical rebuild).

use crate::project::{ResolutionCache, parse_declaration, project_declaration};
use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;

/// Drop all projection tables and replay Declaration episodes in canonical
/// (seq ASC) order. The result must be byte-identical to the incremental
/// projection (tested in the integration test suite).
pub fn reproject(conn: &Connection) -> Result<(), BrainError> {
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
            "SELECT id, space_id, content, ingested_at
             FROM episodes
            WHERE kind = 'declaration' AND redacted_at IS NULL
            ORDER BY seq ASC",
        )
        .map_err(sql_err)?;

    let episodes: Vec<(String, String, String, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, // id
                r.get::<_, String>(1)?, // space_id
                r.get::<_, String>(2)?, // content
                r.get::<_, i64>(3)?,    // ingested_at
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt); // release the prepared statement before we write

    // 3. Replay each declaration, passing its ORIGINAL ingested_at as the
    //    transaction time. This reproduces the exact occurred_at/recorded_at/
    //    episode ids from the incremental path — required for byte-identical
    //    output. project_declaration is idempotent (INSERT OR IGNORE), so
    //    re-inserting rows is a no-op.
    //
    //    Shared `ResolutionCache` so the LSH blocking index is built once per
    //    (space, type) for the whole reproject (M9 §10.1 — sublinear per
    //    mention amortized, not just per individual resolution).
    let mut cache = ResolutionCache::new();
    for (_ep_id, space, content, ingested_at) in &episodes {
        let decl = parse_declaration(content)?;
        project_declaration(conn, space, &decl, Timestamp(*ingested_at), &mut cache)?;
    }
    drop(episodes);

    // 3.5. Replay extractions from cache (deterministic — no LLM call).
    //      Reads cached raw responses and re-projects their claims.
    //      Canonical order: (episode.seq, extractor_id).
    let mut ext_stmt = conn
        .prepare(
            "SELECT e.id, e.space_id, e.content, e.ingested_at,
                    x.extractor_id, x.raw_response
             FROM extractions x
             JOIN episodes e ON x.episode_id = e.id
            WHERE e.kind = 'primary' AND e.redacted_at IS NULL
            ORDER BY e.seq ASC, x.extractor_id ASC",
        )
        .map_err(sql_err)?;

    let extractions: Vec<(String, String, String, i64, String, String)> = ext_stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, // episode_id
                r.get::<_, String>(1)?, // space_id
                r.get::<_, String>(2)?, // content
                r.get::<_, i64>(3)?,    // ingested_at
                r.get::<_, String>(4)?, // extractor_id
                r.get::<_, String>(5)?, // raw_response
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(ext_stmt);

    for (ep_id, space, content, ingested_at, extractor_id, raw) in &extractions {
        crate::extraction::project_from_cache(
            conn,
            space,
            ep_id,
            extractor_id,
            raw,
            content,
            Timestamp(*ingested_at),
            &mut cache,
        )?;
    }
    drop(extractions);

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

    Ok(())
}
