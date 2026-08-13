//! Knowledge-zone writes/reads: entities, keys, merges, statements, assertions,
//! mentions, beliefs (DESIGN §5.7). All take a `&Connection` so they compose
//! inside one writer-actor transaction.

use crate::sql_err;
use oxibrain_core::fold::StatementEntry;
use oxibrain_core::knowledge::{KeyOrigin, Object, Polarity};
use oxibrain_core::{
    Assertion, Belief, BeliefStatus, Entity, EntityKey, EntityMerge, Mention, MergeDecision,
    Statement, Support,
};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

// ── Entities ───────────────────────────────────────────────────────────

pub fn insert_entity(conn: &Connection, e: &Entity) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO entities (id, space_id, type_name, canonical_key, created_at, merged_into)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![e.id, e.space, e.ty, e.canonical_key, e.created_at.millis(), e.merged_into],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn get_entity(conn: &Connection, id: &str) -> Result<Option<Entity>, BrainError> {
    let row = conn.query_row(
        "SELECT id, space_id, type_name, canonical_key, created_at, merged_into
         FROM entities WHERE id = ?1",
        params![id],
        |r| {
            Ok(Entity {
                id: r.get(0)?,
                space: r.get(1)?,
                ty: r.get(2)?,
                canonical_key: r.get(3)?,
                created_at: oxibrain_ports::Timestamp(r.get::<_, i64>(4)?),
                merged_into: r.get(5)?,
            })
        },
    );
    match row {
        Ok(e) => Ok(Some(e)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(sql_err(e)),
    }
}

/// Follow the merged_into chain to the ultimate winner. Path-compresses in memory.
pub fn resolve_entity(conn: &Connection, id: &str) -> Result<String, BrainError> {
    let mut current = id.to_string();
    let mut visited = vec![current.clone()];
    loop {
        let next: Option<String> = {
            let mut stmt = conn
                .prepare("SELECT merged_into FROM entities WHERE id = ?1")
                .map_err(sql_err)?;
            let row = stmt.query_row(params![&current], |r| r.get::<_, Option<String>>(0));
            match row {
                Ok(Some(target)) => Some(target),
                Ok(None) => None,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(BrainError::NotFound(format!("entity {current}")));
                }
                Err(e) => return Err(sql_err(e)),
            }
        };
        match next {
            Some(target) => {
                if visited.contains(&target) {
                    return Err(BrainError::Corruption(format!(
                        "merge cycle: {} → {}",
                        current, target
                    )));
                }
                visited.push(target.clone());
                current = target;
            }
            None => break,
        }
    }
    Ok(current)
}

/// List entities in a space (not merged-away), newest-first, up to `limit`.
/// Used by the `space://` resource and `review_merges`.
pub fn list_entities(
    conn: &Connection,
    space: &str,
    limit: usize,
) -> Result<Vec<Entity>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, type_name, canonical_key, created_at, merged_into
             FROM entities WHERE space_id = ?1 AND merged_into IS NULL
             ORDER BY created_at DESC LIMIT ?2",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, limit as i64], |r| {
            Ok(Entity {
                id: r.get(0)?,
                space: r.get(1)?,
                ty: r.get(2)?,
                canonical_key: r.get(3)?,
                created_at: oxibrain_ports::Timestamp(r.get::<_, i64>(4)?),
                merged_into: r.get(5)?,
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

// ── Entity keys ──────────────────────────────────────────────────────────

pub fn insert_entity_key(conn: &Connection, k: &EntityKey) -> Result<bool, BrainError> {
    let n = conn
        .execute(
            "INSERT OR IGNORE INTO entity_keys (id, space_id, entity_id, type_name, normalized, surface, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![k.id, k.space, k.entity, k.ty, k.normalized, k.surface, k.origin.as_db()],
        )
        .map_err(sql_err)?;
    Ok(n > 0)
}

/// Find entity keys by normalized name + type (exact match).
pub fn find_keys_exact(
    conn: &Connection,
    space: &str,
    ty: &str,
    normalized: &str,
) -> Result<Vec<EntityKey>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, entity_id, type_name, normalized, surface, origin
             FROM entity_keys WHERE space_id = ?1 AND type_name = ?2 AND normalized = ?3",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, ty, normalized], |r| {
            Ok(EntityKey {
                id: r.get(0)?,
                space: r.get(1)?,
                entity: r.get(2)?,
                ty: r.get(3)?,
                normalized: r.get(4)?,
                surface: r.get(5)?,
                origin: KeyOrigin::parse_db(&r.get::<_, String>(6)?).expect("valid origin in db"),
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

/// Get all keys for an entity type in a space (lexical candidate blocking).
pub fn find_keys_for_type(
    conn: &Connection,
    space: &str,
    ty: &str,
) -> Result<Vec<EntityKey>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, entity_id, type_name, normalized, surface, origin
             FROM entity_keys WHERE space_id = ?1 AND type_name = ?2",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, ty], |r| {
            Ok(EntityKey {
                id: r.get(0)?,
                space: r.get(1)?,
                entity: r.get(2)?,
                ty: r.get(3)?,
                normalized: r.get(4)?,
                surface: r.get(5)?,
                origin: KeyOrigin::parse_db(&r.get::<_, String>(6)?).expect("valid origin in db"),
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

// ── Entity merges ──────────────────────────────────────────────────────────

pub fn insert_merge(conn: &Connection, m: &EntityMerge) -> Result<(), BrainError> {
    let (decided_by, score) = m.decided_by.db_columns();
    conn.execute(
        "INSERT OR IGNORE INTO entity_merges (id, loser_id, winner_id, decided_by, score, provenance, decided_at, undone_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![m.id, m.loser, m.winner, decided_by, score, m.provenance, m.decided_at.millis(), m.undone_at.map(|t| t.millis())],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn set_merged_into(conn: &Connection, loser: &str, winner: &str) -> Result<(), BrainError> {
    conn.execute(
        "UPDATE entities SET merged_into = ?1 WHERE id = ?2",
        params![winner, loser],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// List merge records for entities in a space, most recent first.
/// Used by the `review_merges` tool and MCP `review_merges`.
pub fn list_merges(conn: &Connection, space: &str) -> Result<Vec<EntityMerge>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.loser_id, m.winner_id, m.decided_by, m.score,
                    m.provenance, m.decided_at, m.undone_at
             FROM entity_merges m
             JOIN entities e ON e.id = m.loser_id
             WHERE e.space_id = ?1
             ORDER BY m.decided_at DESC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            let decided_by = r.get::<_, String>(3)?;
            let score: Option<f64> = r.get(4).ok();
            Ok(EntityMerge {
                id: r.get(0)?,
                loser: r.get(1)?,
                winner: r.get(2)?,
                decided_by: MergeDecision::parse_db(&decided_by, score)
                    .expect("valid decided_by in db"),
                provenance: r.get(5)?,
                evidence: Vec::new(),
                decided_at: oxibrain_ports::Timestamp(r.get::<_, i64>(6)?),
                undone_at: r.get::<_, Option<i64>>(7)?.map(oxibrain_ports::Timestamp),
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

// ── Statements ──────────────────────────────────────────────────────────

pub fn insert_statement(conn: &Connection, s: &Statement) -> Result<(), BrainError> {
    let (object_entity, object_literal): (Option<&str>, Option<String>) = match &s.object {
        Object::Entity(id) => (Some(id.as_str()), None),
        Object::Literal(tv) => (
            None,
            Some(serde_json::to_string(tv).expect("typed value serializable")),
        ),
    };
    conn.execute(
        "INSERT OR IGNORE INTO statements (id, space_id, subject_id, predicate, object_entity, object_literal)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![s.id, s.space, s.subject, s.predicate, object_entity, object_literal],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Load all statements + their assertions for a (subject, predicate) group.
pub fn get_statement_group(
    conn: &Connection,
    space: &str,
    subject: &str,
    predicate: &str,
) -> Result<Vec<StatementEntry>, BrainError> {
    let mut stmt_q = conn
        .prepare(
            "SELECT id, subject_id, predicate, object_entity, object_literal
             FROM statements WHERE space_id = ?1 AND subject_id = ?2 AND predicate = ?3",
        )
        .map_err(sql_err)?;

    let rows = stmt_q
        .query_map(params![space, subject, predicate], |row| {
            let id: String = row.get(0)?;
            let subject_id: String = row.get(1)?;
            let predicate: String = row.get(2)?;
            let object_entity: Option<String> = row.get(3)?;
            let object_literal: Option<String> = row.get(4)?;
            let object = match (object_entity, object_literal) {
                (Some(e), None) => Object::Entity(e),
                (None, Some(l)) => {
                    Object::Literal(serde_json::from_str(&l).expect("valid literal in db"))
                }
                _ => unreachable!("CHECK constraint guarantees exactly one non-null"),
            };
            Ok(Statement {
                id,
                space: space.to_string(),
                subject: subject_id,
                predicate,
                object,
            })
        })
        .map_err(sql_err)?;

    let mut entries = Vec::new();
    for row_result in rows {
        let statement = row_result.map_err(sql_err)?;
        let assertions = get_assertions_for_statement(conn, &statement.id)?;
        if !assertions.is_empty() {
            entries.push(StatementEntry {
                statement,
                assertions,
            });
        }
    }
    Ok(entries)
}

// ── Assertions ────────────────────────────────────────────────────────────

pub fn insert_assertion(conn: &Connection, a: &Assertion) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO assertions (id, statement_id, episode_id, extractor_id, polarity, claimed_from, claimed_to, confidence, recorded_at, retracted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            a.id,
            a.statement,
            a.episode,
            a.extractor,
            a.polarity.as_db(),
            a.claimed_from.millis(),
            a.claimed_to.millis(),
            a.confidence,
            a.recorded_at.millis(),
            a.retracted_at.map(|t| t.millis()),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn get_assertions_for_statement(
    conn: &Connection,
    statement_id: &str,
) -> Result<Vec<Assertion>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, statement_id, episode_id, extractor_id, polarity,
                    claimed_from, claimed_to, confidence, recorded_at, retracted_at
             FROM assertions WHERE statement_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![statement_id], |r| {
            let polarity_val: i64 = r.get(4)?;
            Ok(Assertion {
                id: r.get(0)?,
                statement: r.get(1)?,
                episode: r.get(2)?,
                extractor: r.get(3)?,
                polarity: Polarity::parse_db(polarity_val).expect("valid polarity in db"),
                claimed_from: oxibrain_ports::Timestamp(r.get::<_, i64>(5)?),
                claimed_to: oxibrain_ports::Timestamp(r.get::<_, i64>(6)?),
                confidence: r.get(7)?,
                recorded_at: oxibrain_ports::Timestamp(r.get::<_, i64>(8)?),
                retracted_at: r.get::<_, Option<i64>>(9)?.map(oxibrain_ports::Timestamp),
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

// ── Mentions ───────────────────────────────────────────────────────────

pub fn insert_mention(conn: &Connection, m: &Mention) -> Result<(), BrainError> {
    let (method_str, _score) = m.method.db_columns();
    conn.execute(
        "INSERT OR IGNORE INTO mentions (id, assertion_id, role, surface, span_start, span_end, resolved_to, method)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            m.id,
            m.assertion,
            m.role.as_db(),
            m.surface,
            m.span.0,
            m.span.1,
            m.resolved_to,
            method_str,
        ],
    )
    .map_err(sql_err)?;
    // Note: the mentions table has no score column in v1/v2. The score is derivable
    // from the method; add a score column in a future migration if read-back needs it.
    Ok(())
}

// ── Beliefs ────────────────────────────────────────────────────────────

/// Replace all beliefs for a set of statements with new beliefs.
/// Deletes old beliefs for the given statement IDs, then inserts the new ones.
pub fn replace_beliefs(
    conn: &Connection,
    statement_ids: &[String],
    beliefs: &[Belief],
) -> Result<(), BrainError> {
    // Delete old beliefs for these statements.
    if !statement_ids.is_empty() {
        let placeholders = std::iter::repeat("?")
            .take(statement_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM beliefs WHERE statement_id IN ({placeholders})");
        let params: Vec<&dyn rusqlite::ToSql> = statement_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        conn.execute(&sql, params.as_slice()).map_err(sql_err)?;
    }

    for b in beliefs {
        let support_json = serde_json::to_string(&b.support).expect("support serializable");
        // Canonicalize the JSON for byte-identical reprojection.
        let support_canon = oxibrain_core::canonical_json_value(
            &serde_json::from_str(&support_json).expect("valid json"),
        );
        conn.execute(
            "INSERT OR REPLACE INTO beliefs (statement_id, valid_from, valid_to, status, confidence, support_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                b.statement,
                b.valid_from.millis(),
                b.valid_to.millis(),
                b.status.as_db(),
                b.confidence,
                support_canon,
            ],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

pub fn get_beliefs_for_statement(
    conn: &Connection,
    statement_id: &str,
) -> Result<Vec<Belief>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT statement_id, valid_from, valid_to, status, confidence, support_json
             FROM beliefs WHERE statement_id = ?1 ORDER BY valid_from",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![statement_id], |r| {
            let support_json: String = r.get(5)?;
            let support: Support =
                serde_json::from_str(&support_json).expect("valid support in db");
            let status_str: String = r.get(3)?;
            Ok(Belief {
                statement: r.get(0)?,
                valid_from: oxibrain_ports::Timestamp(r.get::<_, i64>(1)?),
                valid_to: oxibrain_ports::Timestamp(r.get::<_, i64>(2)?),
                status: BeliefStatus::parse_db(&status_str).expect("valid status in db"),
                confidence: r.get(4)?,
                support,
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}
