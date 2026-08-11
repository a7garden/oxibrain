//! Timeline and diff queries (DESIGN §12.2, §9.6).

use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub statement_id: String,
    pub predicate: String,
    pub object_repr: String,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub status: String,
    pub recorded_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResult {
    pub added: Vec<TimelineEntry>,
    pub removed: Vec<TimelineEntry>,
    pub changed: Vec<TimelineEntry>,
}

/// Belief intervals for an entity over [from, to].
pub fn timeline(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    from: Option<Timestamp>,
    to: Option<Timestamp>,
) -> Result<Vec<TimelineEntry>, BrainError> {
    let from_millis = from.map(|t| t.millis()).unwrap_or(i64::MIN);
    let to_millis = to.map(|t| t.millis()).unwrap_or(i64::MAX);
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.predicate, s.object_entity, s.object_literal,
                    b.valid_from, b.valid_to, b.status,
                    (SELECT MAX(a.recorded_at) FROM assertions a WHERE a.statement_id = s.id) AS recorded_at
             FROM beliefs b
             JOIN statements s ON b.statement_id = s.id
             WHERE s.space_id = ?1 AND s.subject_id = ?2
               AND b.valid_from <= ?3 AND b.valid_to >= ?4
             ORDER BY b.valid_from",
        )
        .map_err(sql_err)?;
    let entries = stmt
        .query_map(params![space, entity_id, to_millis, from_millis], |r| {
            let object_entity: Option<String> = r.get(2)?;
            let object_literal: Option<String> = r.get(3)?;
            let object_repr = object_entity.or(object_literal).unwrap_or_default();
            let recorded_at: Option<i64> = r.get(7)?;
            Ok(TimelineEntry {
                statement_id: r.get(0)?,
                predicate: r.get(1)?,
                object_repr,
                valid_from: Timestamp(r.get::<_, i64>(4)?),
                valid_to: Timestamp(r.get::<_, i64>(5)?),
                status: r.get(6)?,
                recorded_at: Timestamp(recorded_at.unwrap_or(0)),
            })
        })
        .map_err(sql_err)?;
    let mut results = Vec::new();
    for entry in entries {
        results.push(entry.map_err(sql_err)?);
    }
    Ok(results)
}

/// What changed for an entity between two time points.
pub fn diff(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    at_a: Timestamp,
    at_b: Timestamp,
) -> Result<DiffResult, BrainError> {
    let beliefs_a = beliefs_at(conn, space, entity_id, at_a)?;
    let beliefs_b = beliefs_at(conn, space, entity_id, at_b)?;
    let map_a: std::collections::HashMap<String, &TimelineEntry> =
        beliefs_a.iter().map(|e| (e.statement_id.clone(), e)).collect();
    let map_b: std::collections::HashMap<String, &TimelineEntry> =
        beliefs_b.iter().map(|e| (e.statement_id.clone(), e)).collect();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (id, entry_b) in &map_b {
        match map_a.get(id) {
            None => added.push((*entry_b).clone()),
            Some(entry_a)
                if entry_a.status != entry_b.status
                    || entry_a.valid_from != entry_b.valid_from =>
            {
                changed.push((*entry_b).clone());
            }
            _ => {}
        }
    }
    for (id, entry_a) in &map_a {
        if !map_b.contains_key(id) {
            removed.push((*entry_a).clone());
        }
    }
    added.sort_by(|a, b| a.statement_id.cmp(&b.statement_id));
    removed.sort_by(|a, b| a.statement_id.cmp(&b.statement_id));
    changed.sort_by(|a, b| a.statement_id.cmp(&b.statement_id));
    Ok(DiffResult {
        added,
        removed,
        changed,
    })
}

fn beliefs_at(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    at: Timestamp,
) -> Result<Vec<TimelineEntry>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.predicate, s.object_entity, s.object_literal,
                    b.valid_from, b.valid_to, b.status,
                    (SELECT MAX(a.recorded_at) FROM assertions a WHERE a.statement_id = s.id)
             FROM beliefs b
             JOIN statements s ON b.statement_id = s.id
             WHERE s.space_id = ?1 AND s.subject_id = ?2
               AND b.valid_from <= ?3 AND b.valid_to >= ?3
             ORDER BY s.id",
        )
        .map_err(sql_err)?;
    let entries = stmt
        .query_map(params![space, entity_id, at.millis()], |r| {
            let object_entity: Option<String> = r.get(2)?;
            let object_literal: Option<String> = r.get(3)?;
            let recorded_at: Option<i64> = r.get(7)?;
            Ok(TimelineEntry {
                statement_id: r.get(0)?,
                predicate: r.get(1)?,
                object_repr: object_entity.or(object_literal).unwrap_or_default(),
                valid_from: Timestamp(r.get::<_, i64>(4)?),
                valid_to: Timestamp(r.get::<_, i64>(5)?),
                status: r.get(6)?,
                recorded_at: Timestamp(recorded_at.unwrap_or(0)),
            })
        })
        .map_err(sql_err)?;
    let mut results = Vec::new();
    for entry in entries {
        results.push(entry.map_err(sql_err)?);
    }
    Ok(results)
}
