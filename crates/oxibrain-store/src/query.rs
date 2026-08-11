//! Read queries: beliefs for an entity, as-of queries, contradictions.

use crate::knowledge as kcrud;
use crate::sql_err;
use oxibrain_core::knowledge::Object;
use oxibrain_core::{Belief, Statement};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{params, Connection};

/// Current beliefs where `entity` is the subject (follows merge chain).
pub fn beliefs_for_entity(
    conn: &Connection,
    space: &str,
    entity: &str,
) -> Result<Vec<Belief>, BrainError> {
    // Follow merge chain: collect all entities merged into `entity`.
    let entity_ids = collect_merge_group(conn, entity)?;

    let mut beliefs = Vec::new();
    for eid in &entity_ids {
        // Find statements where this entity is the subject.
        let stmt_ids = statement_ids_for_subject(conn, space, eid)?;
        for sid in &stmt_ids {
            beliefs.extend(kcrud::get_beliefs_for_statement(conn, sid)?);
        }
    }
    Ok(beliefs)
}

/// Beliefs as of a valid-time and/or transaction-time point.
/// If `valid_at` is None, all valid-times. If `transaction_at` is None, current.
pub fn beliefs_as_of(
    conn: &Connection,
    space: &str,
    entity: &str,
    valid_at: Option<Timestamp>,
    transaction_at: Option<Timestamp>,
) -> Result<Vec<Belief>, BrainError> {
    // M1: return current beliefs filtered by valid_at.
    // Full transaction-time replay is M2 (timeline/diff).
    let mut beliefs = beliefs_for_entity(conn, space, entity)?;
    if let Some(vt) = valid_at {
        beliefs.retain(|b| b.valid_from <= vt && vt <= b.valid_to);
    }
    let _ = transaction_at; // M2: replay assertion log at this transaction time
    Ok(beliefs)
}

/// All contradicted statements in a space.
pub fn contradictions(conn: &Connection, space: &str) -> Result<Vec<Statement>, BrainError> {
    let mut stmt_q = conn
        .prepare(
            "SELECT DISTINCT s.id, s.space_id, s.subject_id, s.predicate,
                    s.object_entity, s.object_literal
             FROM beliefs b
             JOIN statements s ON b.statement_id = s.id
             WHERE s.space_id = ?1 AND b.status = 'contradicted'",
        )
        .map_err(sql_err)?;

    let rows = stmt_q
        .query_map(params![space], |row| {
            let id: String = row.get(0)?;
            let space_id: String = row.get(1)?;
            let subject: String = row.get(2)?;
            let predicate: String = row.get(3)?;
            let object_entity: Option<String> = row.get(4)?;
            let object_literal: Option<String> = row.get(5)?;
            let object = match (object_entity, object_literal) {
                (Some(e), None) => Object::Entity(e),
                (None, Some(l)) => {
                    Object::Literal(serde_json::from_str(&l).expect("valid literal"))
                }
                _ => unreachable!("CHECK constraint"),
            };
            Ok(Statement {
                id,
                space: space_id,
                subject,
                predicate,
                object,
            })
        })
        .map_err(sql_err)?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

/// Collect all entity ids in the merge group of `entity` (the entity itself
/// plus all entities that merged into it, transitively).
fn collect_merge_group(conn: &Connection, entity: &str) -> Result<Vec<String>, BrainError> {
    let mut group = vec![entity.to_string()];
    // Find all entities whose merged_into chain leads to `entity`.
    // Simple approach: scan for entities with merged_into pointing to any member.
    loop {
        let mut found = Vec::new();
        for member in &group {
            let mut stmt = conn
                .prepare("SELECT id FROM entities WHERE merged_into = ?1")
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(params![member], |r| r.get::<_, String>(0))
                .map_err(sql_err)?;
            for row in rows {
                let id = row.map_err(sql_err)?;
                if !group.contains(&id) && !found.contains(&id) {
                    found.push(id);
                }
            }
        }
        if found.is_empty() {
            break;
        }
        group.extend(found);
    }
    Ok(group)
}

fn statement_ids_for_subject(
    conn: &Connection,
    space: &str,
    entity: &str,
) -> Result<Vec<String>, BrainError> {
    let mut stmt = conn
        .prepare("SELECT id FROM statements WHERE space_id = ?1 AND subject_id = ?2")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, entity], |r| r.get::<_, String>(0))
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}
