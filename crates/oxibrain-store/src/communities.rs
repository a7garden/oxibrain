//! Community clustering via label propagation (DESIGN §9.4).

use crate::query::load_adjacency;
use crate::sql_err;
use oxibrain_index::community::label_propagation;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Rebuild community assignments for a space. Deterministic.
pub fn rebuild_communities(conn: &Connection, space: &str) -> Result<(), BrainError> {
    let graph = load_adjacency(conn, space, None, 0.0)?;
    let map = label_propagation(&graph, 10);
    // Clear and repopulate the communities table.
    conn.execute(
        "DELETE FROM communities WHERE space_id = ?1",
        params![space],
    )
    .map_err(sql_err)?;
    for (entity_id, label) in &map.labels {
        conn.execute(
            "INSERT OR REPLACE INTO communities (id, space_id, label) VALUES (?1, ?2, ?3)",
            params![entity_id, space, label],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Get all entities in the same community as `entity_id`.
pub fn community_members(
    conn: &Connection,
    space: &str,
    entity_id: &str,
) -> Result<Vec<String>, BrainError> {
    let label: Option<i64> = conn
        .query_row(
            "SELECT label FROM communities WHERE id = ?1 AND space_id = ?2",
            params![entity_id, space],
            |r| r.get(0),
        )
        .ok();
    let Some(label) = label else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare("SELECT id FROM communities WHERE space_id = ?1 AND label = ?2 ORDER BY id")
        .map_err(sql_err)?;
    let members = stmt
        .query_map(params![space, label], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    Ok(members)
}
