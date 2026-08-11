//! Index orchestration: FTS5 population, TF-IDF model build, vector persistence.
//! All operations are deterministic functions of the projection data.

use crate::sql_err;
use oxibrain_core::knowledge::{Object, Statement};
use oxibrain_core::object_repr;
#[allow(unused_imports)]
use oxibrain_index::{TfIdfModel, TfIdfVector, tokenize};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Render a statement as a searchable string: "subject predicate object".
/// Uses entity surface names from entity_keys (canonical key).
pub fn render_statement(conn: &Connection, stmt: &Statement) -> Result<String, BrainError> {
    let subject_name = entity_surface(conn, &stmt.subject)?;
    let object_str = match &stmt.object {
        Object::Entity(eid) => entity_surface(conn, eid)?,
        Object::Literal(tv) => object_repr(&Object::Literal(tv.clone())),
    };
    Ok(format!("{subject_name} {} {object_str}", stmt.predicate))
}

fn entity_surface(conn: &Connection, entity_id: &str) -> Result<String, BrainError> {
    // Get the canonical key surface form, or fall back to the entity id.
    let row: Option<(Option<String>,)> = conn
        .query_row(
            "SELECT e.canonical_key
             FROM entities e
             WHERE e.id = ?1",
            params![entity_id],
            |r| Ok((r.get::<_, Option<String>>(0)?,)),
        )
        .map(Some)
        .map_err(sql_err)?;
    match row {
        Some((Some(key_id),)) => {
            let surface: Option<String> = conn
                .query_row(
                    "SELECT surface FROM entity_keys WHERE id = ?1",
                    params![key_id],
                    |r| r.get(0),
                )
                .map_err(sql_err)?;
            Ok(surface.unwrap_or_else(|| entity_id.to_string()))
        }
        _ => {
            // No canonical key — use the first surface from entity_keys.
            let surface: Option<String> = conn
                .query_row(
                    "SELECT surface FROM entity_keys WHERE entity_id = ?1 LIMIT 1",
                    params![entity_id],
                    |r| r.get(0),
                )
                .map_err(sql_err)?;
            Ok(surface.unwrap_or_else(|| entity_id.to_string()))
        }
    }
}

/// Drop and rebuild all FTS5 content for a space.
pub fn rebuild_fts(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute(
        "DELETE FROM episodes_fts WHERE space_id = ?1",
        params![space],
    )
    .map_err(sql_err)?;
    // Index episodes.
    let mut stmt = conn
        .prepare("SELECT id, content FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL")
        .map_err(sql_err)?;
    let episodes: Vec<(String, String)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    for (id, content) in &episodes {
        conn.execute(
            "INSERT INTO episodes_fts (space_id, target_kind, target_id, body)
             VALUES (?1, 'episode', ?2, ?3)",
            params![space, id, content],
        )
        .map_err(sql_err)?;
    }
    // Index statement renderings.
    let statements = load_statements(conn, space)?;
    for stmt in &statements {
        let body = render_statement(conn, stmt)?;
        conn.execute(
            "INSERT INTO episodes_fts (space_id, target_kind, target_id, body)
             VALUES (?1, 'statement', ?2, ?3)",
            params![space, stmt.id, body],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Build TF-IDF model and persist vectors for all episodes + statements in a space.
pub fn rebuild_tfidf(conn: &Connection, space: &str, dim: usize) -> Result<(), BrainError> {
    // Collect all texts.
    let mut texts: Vec<String> = Vec::new();
    let mut targets: Vec<(&str, String)> = Vec::new(); // (kind, id)

    let mut stmt = conn
        .prepare("SELECT id, content FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL")
        .map_err(sql_err)?;
    let episodes: Vec<(String, String)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    for (id, content) in &episodes {
        texts.push(content.clone());
        targets.push(("episode", id.clone()));
    }

    let statements = load_statements(conn, space)?;
    for s in &statements {
        let body = render_statement(conn, s)?;
        texts.push(body);
        targets.push(("statement", s.id.clone()));
    }

    // Fit model.
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let model = TfIdfModel::fit(&text_refs, dim);

    // Persist vectors.
    conn.execute(
        "DELETE FROM tfidf_vectors WHERE space_id = ?1",
        params![space],
    )
    .map_err(sql_err)?;
    for ((kind, id), text) in targets.iter().zip(texts.iter()) {
        let vector = model.transform(text);
        conn.execute(
            "INSERT OR REPLACE INTO tfidf_vectors (space_id, target_kind, target_id, vector)
             VALUES (?1, ?2, ?3, ?4)",
            params![space, kind, id, vector.to_bytes()],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Rebuild salience cache (last_activity per entity).
pub fn rebuild_salience(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute(
        "UPDATE entities SET last_activity = (
            SELECT MAX(a.recorded_at)
            FROM assertions a
            JOIN statements s ON a.statement_id = s.id
            WHERE s.subject_id = entities.id AND s.space_id = ?1
         ) WHERE entities.space_id = ?1",
        params![space],
    )
    .map_err(sql_err)?;
    Ok(())
}
/// Snapshot the index tables (FTS, TF-IDF vectors, communities) for a space
/// into a single deterministic string. Used by determinism tests to assert
/// that reproject produces byte-identical derived state.
pub fn snapshot_indexes(conn: &Connection, space: &str) -> Result<String, BrainError> {
    let mut out = String::new();
    for (label, sql) in [
        (
            "fts",
            "SELECT target_kind, target_id, body FROM episodes_fts WHERE space_id = ?1 ORDER BY target_kind, target_id",
        ),
        (
            "vec",
            "SELECT target_kind, target_id, hex(vector) FROM tfidf_vectors WHERE space_id = ?1 ORDER BY target_kind, target_id",
        ),
        (
            "com",
            "SELECT id, label FROM communities WHERE space_id = ?1 ORDER BY id",
        ),
    ] {
        let mut stmt = conn.prepare(sql).map_err(sql_err)?;
        // Each SQL above returns exactly 3 columns.
        const N: usize = 3;
        let rows: Vec<String> = stmt
            .query_map(params![space], |r| {
                let mut parts = Vec::with_capacity(N);
                for i in 0..N {
                    let part = match r.get_ref(i) {
                        Ok(rusqlite::types::ValueRef::Text(t)) => {
                            String::from_utf8_lossy(t).into_owned()
                        }
                        Ok(rusqlite::types::ValueRef::Null) => String::new(),
                        Ok(rusqlite::types::ValueRef::Integer(j)) => j.to_string(),
                        Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                        Ok(rusqlite::types::ValueRef::Blob(b)) => {
                            format!("blob({})", b.len())
                        }
                        Err(_) => String::new(),
                    };
                    parts.push(part);
                }
                Ok(parts.join("|"))
            })
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        drop(stmt);
        out.push_str(&format!("---{label}---\n"));
        for r in rows {
            out.push_str(&r);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Full index rebuild for a space: FTS + TF-IDF + salience.
pub fn rebuild_indexes(conn: &Connection, space: &str) -> Result<(), BrainError> {
    rebuild_fts(conn, space)?;
    rebuild_tfidf(conn, space, 1024)?;
    rebuild_salience(conn, space)?;
    Ok(())
}

/// Load all statements for a space (for rendering/indexing).
fn load_statements(conn: &Connection, space: &str) -> Result<Vec<Statement>, BrainError> {
    use oxibrain_core::knowledge::{Object, TypedValue};
    let mut stmt = conn
        .prepare(
            "SELECT id, subject_id, predicate, object_entity, object_literal
             FROM statements WHERE space_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            let object_entity: Option<String> = r.get(3)?;
            let object_literal: Option<String> = r.get(4)?;
            let object = match (object_entity, object_literal) {
                (Some(eid), None) => Object::Entity(eid),
                (None, Some(lit)) => Object::Literal(TypedValue::Text(lit)),
                _ => Object::Literal(TypedValue::Text(String::new())),
            };
            Ok(Statement {
                id: r.get(0)?,
                space: space.to_string(),
                subject: r.get(1)?,
                predicate: r.get(2)?,
                object,
            })
        })
        .map_err(sql_err)?;
    let mut statements = Vec::new();
    for row in rows {
        statements.push(row.map_err(sql_err)?);
    }
    Ok(statements)
}
