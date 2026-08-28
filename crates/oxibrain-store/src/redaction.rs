//! Redaction: the only true delete (DESIGN §11.5).
//!
//! Redaction resolves a closure of affected objects, writes an audit entry
//! **before** acting, tombstones content, deletes assertions/mentions/statements,
//! re-folds beliefs, and records the operation in `redactions` so reprojection
//! can replay it.
//!
//! P1 interaction: episode-scoped redaction is handled by `redacted_at IS NULL`
//! filters on the reproject queries (redacted episodes are simply skipped).
//! Entity-scoped redaction survives reproject because the `redactions` table
//! is replayed after extraction replay — the re-created assertions are deleted
//! again, producing the same projection.

use crate::knowledge as kcrud;
use crate::registry;
use crate::sql_err;
use oxibrain_core::confidence::CalibrationTable;
use oxibrain_core::fold::fold;
use oxibrain_core::security::{RedactTarget, RedactionClosure, RedactionResult};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

// ── Closure resolution ──────────────────────────────────────────────────

/// Resolve the closure of objects affected by redacting `target`.
/// Does NOT modify the store — safe for `--dry-run`.
pub fn resolve_closure(
    conn: &Connection,
    target: &RedactTarget,
) -> Result<RedactionClosure, BrainError> {
    match target {
        RedactTarget::Episode { id } => resolve_episode_closure(conn, id),
        RedactTarget::Entity { space, entity_id } => {
            resolve_entity_closure(conn, space, entity_id, None)
        }
        RedactTarget::PredicateScoped {
            space,
            entity_id,
            predicate,
        } => resolve_entity_closure(conn, space, entity_id, Some(predicate)),
        RedactTarget::Space { id } => resolve_space_closure(conn, id),
    }
}

fn resolve_episode_closure(
    conn: &Connection,
    episode_id: &str,
) -> Result<RedactionClosure, BrainError> {
    // Assertions from this episode.
    let assertion_ids = query_strings(
        conn,
        "SELECT id FROM assertions WHERE episode_id = ?1",
        params![episode_id],
    )?;

    // Mentions for those assertions.
    let mention_ids = if assertion_ids.is_empty() {
        Vec::new()
    } else {
        query_strings_in(
            conn,
            "SELECT id FROM mentions WHERE assertion_id IN",
            &assertion_ids,
        )?
    };

    // Statements that will be left unsupported (all assertions come from this episode).
    let unsupported = find_unsupported_for_episode(conn, episode_id)?;

    // Extractions for this episode.
    let extractions = query_strings(
        conn,
        "SELECT episode_id FROM extractions WHERE episode_id = ?1",
        params![episode_id],
    )?;

    Ok(RedactionClosure {
        episodes: vec![episode_id.to_string()],
        assertions: assertion_ids,
        statements: unsupported,
        mentions: mention_ids,
        extractions,
        summaries: Vec::new(),
    })
}

fn resolve_entity_closure(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    predicate_filter: Option<&str>,
) -> Result<RedactionClosure, BrainError> {
    // Find statements involving this entity.
    let stmt_sql = match predicate_filter {
        Some(_) => {
            "SELECT id FROM statements
             WHERE space_id = ?1 AND (subject_id = ?2 OR object_entity = ?2) AND predicate = ?3"
        }
        None => {
            "SELECT id FROM statements
             WHERE space_id = ?1 AND (subject_id = ?2 OR object_entity = ?2)"
        }
    };

    let stmt_ids: Vec<String> = if let Some(pred) = predicate_filter {
        query_strings(conn, stmt_sql, params![space, entity_id, pred])?
    } else {
        query_strings(conn, stmt_sql, params![space, entity_id])?
    };

    if stmt_ids.is_empty() {
        return Ok(RedactionClosure::default());
    }

    // Find assertions for those statements.
    let assertion_ids = query_strings_in(
        conn,
        "SELECT id FROM assertions WHERE statement_id IN",
        &stmt_ids,
    )?;

    // Mentions for those assertions.
    let mention_ids = if assertion_ids.is_empty() {
        Vec::new()
    } else {
        query_strings_in(
            conn,
            "SELECT id FROM mentions WHERE assertion_id IN",
            &assertion_ids,
        )?
    };

    // Unsupported statements: those that lose ALL assertions.
    let unsupported = find_unsupported_statements(conn, &stmt_ids, &assertion_ids)?;

    Ok(RedactionClosure {
        episodes: Vec::new(), // entity redaction does not tombstone episodes
        assertions: assertion_ids,
        statements: unsupported,
        mentions: mention_ids,
        extractions: Vec::new(),
        summaries: Vec::new(),
    })
}

/// Space-level closure (spec §4.5): every episode in the space and every
/// derived row. Enumerated, not folded per-episode — a space purge leaves
/// nothing to refold.
fn resolve_space_closure(
    conn: &Connection,
    space_id: &str,
) -> Result<RedactionClosure, BrainError> {
    let episodes = query_strings(
        conn,
        "SELECT id FROM episodes WHERE space_id = ?1",
        &[&space_id],
    )?;
    let statements = query_strings(
        conn,
        "SELECT id FROM statements WHERE space_id = ?1",
        &[&space_id],
    )?;
    let assertions = if statements.is_empty() {
        Vec::new()
    } else {
        query_strings(
            conn,
            "SELECT a.id FROM assertions a
             JOIN statements s ON s.id = a.statement_id
             WHERE s.space_id = ?1",
            &[&space_id],
        )?
    };
    let mentions = if assertions.is_empty() {
        Vec::new()
    } else {
        query_strings(
            conn,
            "SELECT m.id FROM mentions m
             JOIN assertions a ON a.id = m.assertion_id
             JOIN statements s ON s.id = a.statement_id
             WHERE s.space_id = ?1",
            &[&space_id],
        )?
    };
    let extractions = if episodes.is_empty() {
        Vec::new()
    } else {
        query_strings_in(
            conn,
            "SELECT DISTINCT episode_id FROM extractions WHERE ",
            &episodes,
        )?
    };
    // `summaries` is keyed by (scope_kind, member_set_hash, extractor_id), not
    // by space_id — left to cache-zone rebuild; nothing to enumerate here.
    Ok(RedactionClosure {
        episodes,
        assertions,
        statements,
        mentions,
        extractions,
        summaries: Vec::new(),
    })
}

/// Find statements that have zero assertions outside `delete_ids`.
fn find_unsupported_statements(
    conn: &Connection,
    stmt_ids: &[String],
    delete_assertion_ids: &[String],
) -> Result<Vec<String>, BrainError> {
    let mut unsupported = Vec::new();
    for sid in stmt_ids {
        // Count assertions for this statement not in the delete set.
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assertions WHERE statement_id = ?1",
                params![sid],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        let keeping: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM assertions WHERE statement_id = ?1 AND id NOT IN (SELECT value FROM json_each(?2))",
                params![sid, serde_json::to_string(delete_assertion_ids).unwrap_or_default()],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        if keeping == 0 && total > 0 {
            unsupported.push(sid.clone());
        }
    }
    Ok(unsupported)
}

/// For episode redaction: statements that have all assertions from this episode.
fn find_unsupported_for_episode(
    conn: &Connection,
    episode_id: &str,
) -> Result<Vec<String>, BrainError> {
    // Statements where ALL assertions come from this episode.
    let rows = query_strings(
        conn,
        "SELECT DISTINCT a.statement_id FROM assertions a
         WHERE a.episode_id = ?1
         AND NOT EXISTS (
             SELECT 1 FROM assertions a2
             WHERE a2.statement_id = a.statement_id AND a2.episode_id != ?1
         )",
        params![episode_id],
    )?;
    Ok(rows)
}

// ── Execution ───────────────────────────────────────────────────────────

/// Full space teardown (FK-safe order). Mirrors reproject's end state for a
/// space with no episodes: nothing. Tombstones and audit rows stay.
fn execute_space_redaction(
    conn: &Connection,
    space_id: &str,
    reason: &str,
    actor: &str,
    now: Timestamp,
) -> Result<RedactionResult, BrainError> {
    let closure = resolve_closure(
        conn,
        &RedactTarget::Space {
            id: space_id.to_string(),
        },
    )?;
    // Empty-closure path (e.g. a space with zero episodes — only document
    // chunks): the space row is administrative, not part of the
    // episode-derived closure. Drop it directly and return. No
    // audit/tombstone row is written (nothing was redacted; the row drop
    // is bookkeeping). Idempotent: a second call against a gone space
    // deletes 0 rows and returns an empty closure without error.
    if closure.assertions.is_empty() && closure.episodes.is_empty() {
        conn.execute("DELETE FROM spaces WHERE id = ?1", params![space_id])
            .map_err(sql_err)?;
        return Ok(RedactionResult {
            closure,
            beliefs_refolded: 0,
        });
    }

    // 1. Audit BEFORE acting (§11.5). Same write_audit used by the generic
    //    path; record before any data goes away.
    crate::security::write_audit(
        conn,
        actor,
        None,
        "redact",
        Some(
            &serde_json::to_string(&RedactTarget::Space {
                id: space_id.to_string(),
            })
            .unwrap_or_default(),
        ),
        Some(reason),
        now,
    )?;

    // 2. Append to `redactions` table (INSERT OR IGNORE → idempotent on
    //    repeated audit + replay during reproject).
    let target_json = serde_json::to_string(&RedactTarget::Space {
        id: space_id.to_string(),
    })
    .unwrap_or_default();
    let rid = oxibrain_core::id::token_id(&target_json, now);
    conn.execute(
        "INSERT OR IGNORE INTO redactions (id, target_json, reason, actor, redacted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![rid, target_json, reason, actor, now.millis()],
    )
    .map_err(sql_err)?;

    // 3. Teardown in FK-safe order. Cache/derived tables first (no FKs into
    //    truth tables), then truth half (children before parents).
    //    `fts_word` / `fts_ngram` use column `space_id` (v6 schema —
    //    `episodes_fts` was dropped in v6 and is gone in v11).
    for sql in [
        "DELETE FROM fts_word WHERE space_id = ?1",
        "DELETE FROM fts_ngram WHERE space_id = ?1",
        "DELETE FROM tfidf_vectors WHERE space_id = ?1",
        "DELETE FROM entity_vectors WHERE entity_id IN (SELECT id FROM entities WHERE space_id = ?1)",
        "DELETE FROM communities WHERE space_id = ?1",
        "DELETE FROM chunks WHERE space_id = ?1",
    ] {
        conn.execute(sql, params![space_id]).map_err(sql_err)?;
    }
    for sql in [
        "DELETE FROM extraction_failures WHERE episode_id IN (SELECT id FROM episodes WHERE space_id = ?1)",
        "DELETE FROM extractions WHERE episode_id IN (SELECT id FROM episodes WHERE space_id = ?1)",
        "DELETE FROM episode_links WHERE from_episode IN (SELECT id FROM episodes WHERE space_id = ?1) OR to_episode IN (SELECT id FROM episodes WHERE space_id = ?1)",
        "DELETE FROM mentions WHERE assertion_id IN (SELECT a.id FROM assertions a JOIN statements s ON s.id = a.statement_id WHERE s.space_id = ?1)",
        "DELETE FROM assertions WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1)",
        "DELETE FROM beliefs WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1)",
        "DELETE FROM statements WHERE space_id = ?1",
        "DELETE FROM entity_merges WHERE loser_id IN (SELECT id FROM entities WHERE space_id = ?1) OR winner_id IN (SELECT id FROM entities WHERE space_id = ?1)",
        "DELETE FROM entity_keys WHERE space_id = ?1",
        "DELETE FROM entities WHERE space_id = ?1",
        "DELETE FROM source_policies WHERE source_id IN (SELECT id FROM sources WHERE space_id = ?1)",
        "DELETE FROM sources WHERE space_id = ?1",
        "DELETE FROM episodes WHERE space_id = ?1",
        "DELETE FROM spaces WHERE id = ?1",
    ] {
        conn.execute(sql, params![space_id]).map_err(sql_err)?;
    }

    Ok(RedactionResult {
        closure,
        beliefs_refolded: 0,
    })
}

/// Execute redaction. Writes audit + redactions record FIRST, then tombstones
/// and deletes. Returns what was affected.
pub fn execute_redaction(
    conn: &Connection,
    target: &RedactTarget,
    reason: &str,
    actor: &str,
    now: Timestamp,
) -> Result<RedactionResult, BrainError> {
    // Space purge: dedicated teardown path. Bypasses the generic
    // closure-driven flow because every space-scoped row goes together —
    // there is nothing to refold and tombstones are unnecessary (the space
    // itself is dropped). Idempotent: a second call sees an empty closure
    // and short-circuits.
    if let RedactTarget::Space { id } = target {
        return execute_space_redaction(conn, id, reason, actor, now);
    }

    // 1. Resolve closure.
    let closure = resolve_closure(conn, target)?;
    if closure.assertions.is_empty() && closure.episodes.is_empty() {
        // Idempotent: nothing to do.
        return Ok(RedactionResult {
            closure,
            beliefs_refolded: 0,
        });
    }

    // 2. Write audit BEFORE acting (§11.5).
    crate::security::write_audit(
        conn,
        actor,
        None,
        "redact",
        Some(&serde_json::to_string(target).unwrap_or_default()),
        Some(reason),
        now,
    )?;

    // 3. Record in redactions table (for reproject replay).
    let target_json = serde_json::to_string(target).unwrap_or_default();
    let rid = oxibrain_core::id::token_id(&target_json, now); // reuse hash+time id
    conn.execute(
        "INSERT OR IGNORE INTO redactions (id, target_json, reason, actor, redacted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![rid, target_json, reason, actor, now.millis()],
    )
    .map_err(sql_err)?;

    // 4. Tombstone episode content and extractions (episode-scoped only).
    for ep_id in &closure.episodes {
        conn.execute(
            "UPDATE episodes SET content = '[redacted]', redacted_at = ?1 WHERE id = ?2 AND redacted_at IS NULL",
            params![now.millis(), ep_id],
        )
        .map_err(sql_err)?;
    }
    for ep_id in &closure.extractions {
        conn.execute(
            "UPDATE extractions SET raw_response = '[redacted]' WHERE episode_id = ?1",
            params![ep_id],
        )
        .map_err(sql_err)?;
    }

    // 5. Delete mentions for affected assertions.
    delete_in(
        conn,
        "DELETE FROM mentions WHERE assertion_id IN",
        &closure.assertions,
    )?;

    // 6. Delete assertions.
    delete_in(
        conn,
        "DELETE FROM assertions WHERE id IN",
        &closure.assertions,
    )?;

    // 7. Delete unsupported statements.
    if !closure.statements.is_empty() {
        delete_in(
            conn,
            "DELETE FROM beliefs WHERE statement_id IN",
            &closure.statements,
        )?;
        delete_in(
            conn,
            "DELETE FROM statements WHERE id IN",
            &closure.statements,
        )?;
    }

    // 8. Re-fold affected belief groups (statements that lost some assertions
    //    but still have remaining ones).
    let beliefs_refolded = refold_affected(conn, target, now)?;

    Ok(RedactionResult {
        closure,
        beliefs_refolded,
    })
}

/// Replay a redaction during reproject. Deletes the affected assertions and
/// re-folds. Does NOT tombstone (content/extractions are already filtered by
/// `redacted_at IS NULL` in the replay queries) or write audit (already done).
/// `at` is the original redaction timestamp — used as the fold's reference
/// point so assertions with `recorded_at <= at` are visible.
pub fn apply_replay(
    conn: &Connection,
    target: &RedactTarget,
    at: Timestamp,
) -> Result<usize, BrainError> {
    // Space purges never replay: `execute_space_redaction` physically
    // deletes the purged space's episodes — no tombstoned rows remain — so
    // replay cannot legitimately resurrect anything. Space ids are
    // deterministic (`ledger::space_id`), so a live space bearing the
    // redacted id can only be a post-purge re-add; replaying against it
    // would silently destroy the re-added space's projection on every
    // reproject, forever.
    if matches!(target, RedactTarget::Space { .. }) {
        return Ok(0);
    }

    let closure = resolve_closure(conn, target)?;
    if closure.assertions.is_empty() {
        return Ok(0);
    }

    // Delete mentions.
    delete_in(
        conn,
        "DELETE FROM mentions WHERE assertion_id IN",
        &closure.assertions,
    )?;

    // Delete assertions.
    delete_in(
        conn,
        "DELETE FROM assertions WHERE id IN",
        &closure.assertions,
    )?;

    // Delete unsupported statements.
    if !closure.statements.is_empty() {
        delete_in(
            conn,
            "DELETE FROM beliefs WHERE statement_id IN",
            &closure.statements,
        )?;
        delete_in(
            conn,
            "DELETE FROM statements WHERE id IN",
            &closure.statements,
        )?;
    }

    // Re-fold affected groups using the original redaction timestamp.
    let refolded = refold_affected(conn, target, at)?;
    Ok(refolded)
}

/// Re-fold belief groups for statements that still have assertions after
/// the redaction deleted some.
fn refold_affected(
    conn: &Connection,
    target: &RedactTarget,
    now: Timestamp,
) -> Result<usize, BrainError> {
    let (space, entity_id, predicate_filter) = match target {
        RedactTarget::Episode { .. } => {
            // For episode redaction, re-fold all groups that had assertions
            // from this episode. Find the affected (subject, predicate) pairs.
            refold_episode_groups(conn, target, now)?;
            return Ok(0); // count computed inside refold_episode_groups
        }
        RedactTarget::Entity { space, entity_id } => (space.as_str(), entity_id.as_str(), None),
        RedactTarget::PredicateScoped {
            space,
            entity_id,
            predicate,
        } => (space.as_str(), entity_id.as_str(), Some(predicate.as_str())),
        // Space purge short-circuits in execute_redaction and is replayed as
        // a no-op (closure is empty once the space is gone), so this arm is
        // only reached defensively.
        RedactTarget::Space { .. } => return Ok(0),
    };

    // Find distinct (subject_id, predicate) groups for statements involving
    // the entity that still have assertions.
    let groups_sql = match predicate_filter {
        Some(_) => {
            "SELECT DISTINCT subject_id, predicate FROM statements
             WHERE space_id = ?1 AND (subject_id = ?2 OR object_entity = ?2) AND predicate = ?3"
        }
        None => {
            "SELECT DISTINCT subject_id, predicate FROM statements
             WHERE space_id = ?1 AND (subject_id = ?2 OR object_entity = ?2)"
        }
    };

    let groups: Vec<(String, String)> = if let Some(pred) = predicate_filter {
        let mut stmt = conn.prepare(groups_sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space, entity_id, pred], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(sql_err)?);
        }
        result
    } else {
        let mut stmt = conn.prepare(groups_sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space, entity_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(sql_err)?);
        }
        result
    };

    let mut count = 0;
    for (subj, pred) in groups {
        let group = kcrud::get_statement_group(conn, space, &subj, &pred)?;
        let group_stmt_ids: Vec<String> = group.iter().map(|e| e.statement.id.clone()).collect();

        if group.is_empty() {
            // No assertions left — delete beliefs for these statements.
            if !group_stmt_ids.is_empty() {
                delete_in(
                    conn,
                    "DELETE FROM beliefs WHERE statement_id IN",
                    &group_stmt_ids,
                )?;
            }
        } else {
            let calibration = CalibrationTable::default();
            let pred_def = registry::load_predicate(conn, &pred)?
                .ok_or_else(|| BrainError::Invalid(format!("unknown predicate: {pred}")))?;
            let beliefs = fold(&pred_def, &group, now, &calibration);
            kcrud::replace_beliefs(conn, &group_stmt_ids, &beliefs)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Re-fold groups affected by an episode redaction. Finds all (subject,
/// predicate) pairs that had assertions from the redacted episode.
fn refold_episode_groups(
    conn: &Connection,
    target: &RedactTarget,
    now: Timestamp,
) -> Result<usize, BrainError> {
    let episode_id = match target {
        RedactTarget::Episode { id } => id.as_str(),
        _ => return Ok(0),
    };

    // The assertions are already deleted. Find groups that still have statements
    // but lost assertions from this episode. We look at the space and all
    // (subject, predicate) pairs that existed for this episode's assertions.
    // Since assertions are deleted, we can't query them directly. Instead,
    // find all groups in the episode's space and re-fold those that have
    // statements with remaining assertions.
    //
    // Simpler approach: find all distinct (space, subject_id, predicate) from
    // statements in the episode's space, and re-fold any group that has at
    // least one assertion. This is broader than necessary but correct —
    // re-folding is idempotent.
    //
    // Actually, even simpler: after episode redaction, the episode's assertions
    // are gone. Any statement that had ONLY this episode's assertions is deleted.
    // Statements that had other episodes' assertions survive with fewer assertions.
    // We need to re-fold those groups.
    //
    // Since we don't know which groups were affected (assertions deleted), we
    // can find the affected groups by looking at which statements still exist
    // but in the same space. This is expensive. Instead, let's just re-fold
    // all groups in the affected space. Reprojection already does this for
    // indexes. For incremental redaction, it's a one-time cost.
    //
    // PRAGMATIC: for M4, episode-scoped redaction is the less common case.
    // Re-fold all groups in the space. This is correct if slow.

    // Find the space for this episode.
    let space: Option<String> = conn
        .query_row(
            "SELECT space_id FROM episodes WHERE id = ?1",
            params![episode_id],
            |r| r.get(0),
        )
        .ok();

    let Some(space) = space else {
        return Ok(0);
    };

    // Re-fold all groups in this space.
    let groups: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT subject_id, predicate FROM statements WHERE space_id = ?1")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(sql_err)?);
        }
        result
    };

    let mut count = 0;
    for (subj, pred) in groups {
        let group = kcrud::get_statement_group(conn, &space, &subj, &pred)?;
        let group_stmt_ids: Vec<String> = group.iter().map(|e| e.statement.id.clone()).collect();

        if group.is_empty() {
            if !group_stmt_ids.is_empty() {
                delete_in(
                    conn,
                    "DELETE FROM beliefs WHERE statement_id IN",
                    &group_stmt_ids,
                )?;
            }
        } else {
            let calibration = CalibrationTable::default();
            let pred_def = registry::load_predicate(conn, &pred)?
                .ok_or_else(|| BrainError::Invalid(format!("unknown predicate: {pred}")))?;
            let beliefs = fold(&pred_def, &group, now, &calibration);
            kcrud::replace_beliefs(conn, &group_stmt_ids, &beliefs)?;
            count += 1;
        }
    }
    Ok(count)
}

// ── SQL helpers ─────────────────────────────────────────────────────────

fn query_strings(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<String>, BrainError> {
    let mut stmt = conn.prepare(sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(params, |r| r.get::<_, String>(0))
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

fn query_strings_in(
    conn: &Connection,
    prefix: &str,
    ids: &[String],
) -> Result<Vec<String>, BrainError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("{prefix} ({placeholders})");
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    query_strings(conn, &sql, &params)
}

fn delete_in(conn: &Connection, prefix: &str, ids: &[String]) -> Result<(), BrainError> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("{prefix} ({placeholders})");
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, params.as_slice()).map_err(sql_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger;
    use crate::migration;
    use crate::project::{DeclObject, Declaration, EntityRef};
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        // Register sqlite-vec BEFORE opening: vec0 virtual tables (v5+)
        // need the auto-extension in place at open time, and this module
        // must not depend on some other test module having registered it.
        crate::migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().expect("open");
        migration::run(&conn).expect("migrate");
        // Ensure a space exists for FK constraints.
        crate::ledger::create_space(&conn, "personal", Timestamp::from_millis(0))
            .expect("create space");
        conn
    }

    fn declare_alice_works_for_acme(conn: &Connection, now: Timestamp) -> String {
        let sid = crate::ledger::create_space(conn, "personal", Timestamp::from_millis(0))
            .expect("ensure space");
        declare_in(conn, &sid, now)
    }

    /// Declare `Alice employed_by Acme` into an arbitrary space; returns the
    /// declaration episode id.
    fn declare_in(conn: &Connection, space: &str, now: Timestamp) -> String {
        let decl = Declaration::AddStatement {
            subject: EntityRef {
                surface: "Alice".into(),
                ty: "person".into(),
            },
            predicate: "employed_by".into(),
            object: DeclObject::Entity {
                surface: "Acme".into(),
                ty: "organization".into(),
            },
            polarity: "affirm".into(),
            valid_from: 0,
            valid_to: oxibrain_ports::TIME_MAX.millis(),
        };
        let mut cache = crate::project::ResolutionCache::new();
        crate::project::project_declaration(conn, space, &decl, now, &mut cache).expect("declare")
    }

    #[test]
    fn redact_episode_deletes_assertions() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let ep_id = declare_alice_works_for_acme(&conn, now);

        // Verify assertion exists.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Redact the episode.
        let target = RedactTarget::Episode { id: ep_id.clone() };
        let result = execute_redaction(&conn, &target, "test", "tester", now).unwrap();

        assert!(!result.closure.assertions.is_empty());
        assert!(!result.closure.statements.is_empty());

        // Episode content is tombstoned.
        let content: String = conn
            .query_row(
                "SELECT content FROM episodes WHERE id = ?1",
                params![ep_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "[redacted]");

        // Assertions deleted.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Statements deleted (unsupported).
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM statements", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn redact_is_idempotent() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let ep_id = declare_alice_works_for_acme(&conn, now);

        let target = RedactTarget::Episode { id: ep_id };
        let first = execute_redaction(&conn, &target, "test", "tester", now).unwrap();
        assert!(!first.closure.assertions.is_empty());

        // Second call: empty closure, no-op.
        let second = execute_redaction(&conn, &target, "test", "tester", now).unwrap();
        assert!(second.closure.assertions.is_empty());
    }

    #[test]
    fn dry_run_does_not_modify() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let ep_id = declare_alice_works_for_acme(&conn, now);

        let target = RedactTarget::Episode { id: ep_id.clone() };
        let closure = resolve_closure(&conn, &target).unwrap();
        assert!(!closure.assertions.is_empty());

        // Nothing changed.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn redaction_recorded_in_table() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let ep_id = declare_alice_works_for_acme(&conn, now);

        let target = RedactTarget::Episode { id: ep_id };
        execute_redaction(&conn, &target, "gdpr", "admin", now).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM redactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn reproject_after_episode_redaction_preserves_projection() {
        // Redact an episode → reproject → the redacted episode is NOT replayed
        // (redacted_at IS NULL filter), and no beliefs are recreated.
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let ep_id = declare_alice_works_for_acme(&conn, now);

        // Before redaction: 1 assertion, 1 belief.
        let assert_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(assert_before, 1);
        let beliefs_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(beliefs_before, 1);

        // Redact the episode.
        let target = RedactTarget::Episode { id: ep_id };
        execute_redaction(&conn, &target, "test", "tester", now).unwrap();

        // After redaction: 0 assertions, 0 beliefs.
        let assert_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(assert_after, 0);

        // Reproject.
        crate::reproject::reproject(&conn).unwrap();

        // Reproject does NOT recreate the assertions (episode is redacted).
        let assert_reproj: i64 = conn
            .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(assert_reproj, 0);

        // No beliefs either.
        let beliefs_reproj: i64 = conn
            .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(beliefs_reproj, 0);
    }

    #[test]
    fn redact_space_removes_everything_and_audits() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let space_id = ledger::space_id("personal");
        declare_alice_works_for_acme(&conn, now);

        let target = RedactTarget::Space {
            id: space_id.clone(),
        };
        let closure = resolve_closure(&conn, &target).unwrap();
        assert!(!closure.episodes.is_empty());
        assert!(!closure.assertions.is_empty());

        let result = execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
        assert!(!result.closure.episodes.is_empty());

        // Nothing references the space anymore.
        for (table, col) in [
            ("episodes", "space_id"),
            ("entities", "space_id"),
            ("entity_keys", "space_id"),
            ("statements", "space_id"),
            ("communities", "space_id"),
            ("chunks", "space_id"),
        ] {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1"),
                    [&space_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "{table} still has rows for the space");
        }
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM spaces WHERE id = ?1",
                [&space_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "space row must be dropped");
        // Audit trail kept.
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE operation = 'redact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(audited >= 1);
    }

    #[test]
    fn redact_space_is_idempotent() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        declare_alice_works_for_acme(&conn, now);
        let space_id = ledger::space_id("personal");
        let target = RedactTarget::Space { id: space_id };
        execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
        // Capture audit + tombstone counts after the first purge; the
        // idempotent second call must leave them unchanged (no extra
        // audit rows, no duplicate `redactions` tombstones).
        let audit_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        let redactions_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM redactions", [], |r| r.get(0))
            .unwrap();
        assert!(audit_after_first >= 1);
        assert!(redactions_after_first >= 1);
        let second = execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
        assert!(second.closure.episodes.is_empty());
        let audit_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        let redactions_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM redactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            audit_after_second, audit_after_first,
            "idempotent redaction must not re-write audit_log rows"
        );
        assert_eq!(
            redactions_after_second, redactions_after_first,
            "idempotent redaction must not duplicate redactions tombstones"
        );
    }

    #[test]
    fn reproject_after_space_redaction_stays_empty() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        declare_alice_works_for_acme(&conn, now);
        let space_id = ledger::space_id("personal");
        execute_redaction(&conn, &RedactTarget::Space { id: space_id }, "t", "t", now).unwrap();
        // Reproject must not resurrect anything for the redacted space.
        crate::reproject::reproject(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn readded_space_survives_reproject_after_purge() {
        // Regression (final review): purge 'dev' → re-add 'dev' (same
        // deterministic id) → re-declare → reproject. The permanent Space
        // tombstone used to replay against the RE-ADDED space's rows and
        // silently delete them — the projection stayed poisoned forever.
        // Replay must leave a live space bearing a purged id untouched.
        let conn = fresh_db();
        let dev = ledger::create_space(&conn, "dev", Timestamp::from_millis(0)).unwrap();
        assert_eq!(dev, ledger::space_id("dev"), "space ids are deterministic");
        declare_in(&conn, &dev, Timestamp::from_millis(1000));

        execute_redaction(
            &conn,
            &RedactTarget::Space { id: dev.clone() },
            "purge",
            "cli",
            Timestamp::from_millis(2000),
        )
        .unwrap();

        // Re-add 'dev' — the deterministic id collides with the tombstone —
        // and declare fresh content into it.
        let dev_again = ledger::create_space(&conn, "dev", Timestamp::from_millis(3000)).unwrap();
        assert_eq!(dev_again, dev);
        declare_in(&conn, &dev, Timestamp::from_millis(4000));

        let counts = |conn: &Connection| -> (i64, i64, i64) {
            let eps: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM episodes WHERE space_id = ?1",
                    params![dev],
                    |r| r.get(0),
                )
                .unwrap();
            let asserts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM assertions a JOIN statements s ON s.id = a.statement_id WHERE s.space_id = ?1",
                    params![dev],
                    |r| r.get(0),
                )
                .unwrap();
            let stmts: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM statements WHERE space_id = ?1",
                    params![dev],
                    |r| r.get(0),
                )
                .unwrap();
            (eps, asserts, stmts)
        };
        assert_eq!(counts(&conn), (1, 1, 1), "probe before reproject");

        crate::reproject::reproject(&conn).unwrap();

        assert_eq!(
            counts(&conn),
            (1, 1, 1),
            "re-added space must survive reproject (episodes/assertions/statements)"
        );
    }

    #[test]
    fn redact_space_on_empty_space_still_drops_row() {
        // A space with NO episodes (e.g. only document chunks — the exact
        // case that forces `--purge`) must still drop its `spaces` row.
        // Without this, `space remove X --purge` would print "removed"
        // while the row survives as a ghost.
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        // Create a second space that has zero episodes. `personal` already
        // exists from `fresh_db()`, but we want an empty target.
        let empty_id = ledger::space_id("empty-space");
        crate::ledger::create_space(&conn, "empty-space", now).unwrap();

        // Sanity: the row exists and has no episodes.
        let pre: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM spaces WHERE id = ?1",
                [&empty_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pre, 1, "empty space row must exist before purge");
        let pre_episodes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE space_id = ?1",
                [&empty_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pre_episodes, 0, "empty space must have zero episodes");

        let target = RedactTarget::Space {
            id: empty_id.clone(),
        };
        let result = execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
        assert!(result.closure.episodes.is_empty());

        // The spaces row MUST be gone even though the closure was empty.
        let post: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM spaces WHERE id = ?1",
                [&empty_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(post, 0, "spaces row must be dropped on empty-space purge");

        // Idempotent: second call returns empty closure without error.
        let audit_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        let redactions_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM redactions", [], |r| r.get(0))
            .unwrap();
        let second = execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
        assert!(second.closure.episodes.is_empty());
        // Audit + redactions counts unchanged (no event was redacted; the
        // empty-space drop is administrative bookkeeping, not a redaction).
        let audit_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        let redactions_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM redactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audit_after_second, audit_after_first);
        assert_eq!(redactions_after_second, redactions_after_first);
    }
}
