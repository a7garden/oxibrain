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

/// Execute redaction. Writes audit + redactions record FIRST, then tombstones
/// and deletes. Returns what was affected.
pub fn execute_redaction(
    conn: &Connection,
    target: &RedactTarget,
    reason: &str,
    actor: &str,
    now: Timestamp,
) -> Result<RedactionResult, BrainError> {
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
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
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
    let placeholders = std::iter::repeat("?")
        .take(ids.len())
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
    use crate::migration;
    use crate::project::{DeclObject, Declaration, EntityRef};
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        migration::run(&conn).expect("migrate");
        // Ensure a space exists for FK constraints.
        crate::ledger::create_space(&conn, "personal", Timestamp::from_millis(0))
            .expect("create space");
        conn
    }

    fn declare_alice_works_for_acme(conn: &Connection, now: Timestamp) -> String {
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
        let sid = crate::ledger::create_space(conn, "personal", Timestamp::from_millis(0))
            .expect("ensure space");
        let mut cache = crate::project::ResolutionCache::new();
        crate::project::project_declaration(conn, &sid, &decl, now, &mut cache).expect("declare")
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
}
