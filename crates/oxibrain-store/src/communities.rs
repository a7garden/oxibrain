//! Community clustering via label propagation (DESIGN §9.4).

use crate::sql_err;
use oxibrain_index::adjacency::WeightedAdjacencyGraph;
use oxibrain_index::community::label_propagation_weighted;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Rebuild community assignments for a space. Deterministic.
pub fn rebuild_communities(conn: &Connection, space: &str) -> Result<(), BrainError> {
    // Build a weighted adjacency graph from mean belief confidence per
    // entity pair (§9.4, 10.6). An Alice→Bob edge with confidence 0.9 carries
    // 3× the label-propagation weight of an edge at 0.3.
    let graph = load_weighted_adjacency(conn, space, None, 0.0)?;
    let map = label_propagation_weighted(&graph, 10);
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

/// Build a weighted adjacency graph from mean belief confidence per entity
/// pair (§9.4, 10.6). Edge weight = AVG(beliefs.confidence) for all
/// active/superseded statements connecting the pair.
fn load_weighted_adjacency(
    conn: &Connection,
    space: &str,
    valid_at: Option<oxibrain_ports::Timestamp>,
    min_confidence: f32,
) -> Result<WeightedAdjacencyGraph, BrainError> {
    let mut graph = WeightedAdjacencyGraph::new();
    let mut sql = String::from(
        "SELECT s.subject_id, s.object_entity, AVG(b.confidence) as avg_conf
         FROM statements s
         INNER JOIN beliefs b ON b.statement_id = s.id
         WHERE s.space_id = ?1
           AND s.object_entity IS NOT NULL
           AND b.status IN ('active', 'superseded')
           AND b.confidence >= ?2",
    );
    if valid_at.is_some() {
        sql.push_str(
            " AND (b.valid_from IS NULL OR b.valid_from <= ?3) \
             AND (b.valid_to IS NULL OR b.valid_to >= ?3)",
        );
    }
    sql.push_str(" GROUP BY s.subject_id, s.object_entity");
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = if let Some(t) = valid_at {
        stmt.query_map(params![space, min_confidence, t.0], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .map_err(sql_err)?
        .collect::<Vec<_>>()
    } else {
        stmt.query_map(params![space, min_confidence], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .map_err(sql_err)?
        .collect::<Vec<_>>()
    };
    for row in rows {
        let r = row.map_err(sql_err)?;
        graph.add_edge(&r.0, &r.1, r.2);
    }
    Ok(graph)
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
