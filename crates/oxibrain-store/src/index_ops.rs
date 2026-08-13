//! Index orchestration: FTS5 population, TF-IDF model build, vector persistence.
//! All operations are deterministic functions of the projection data.

use crate::sql_err;
use oxibrain_core::knowledge::{Object, Statement};
use oxibrain_core::object_repr;
#[allow(unused_imports)]
use oxibrain_index::{TfIdfModel, TfIdfVector, features};
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

/// Drop and rebuild all FTS5 content for a space — both word and trigram
/// indexes (§7.4). Both are always populated; no script detection, no routing.
pub fn rebuild_fts(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute("DELETE FROM fts_word WHERE space_id = ?1", params![space])
        .map_err(sql_err)?;
    conn.execute("DELETE FROM fts_ngram WHERE space_id = ?1", params![space])
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
            "INSERT INTO fts_word (body, space_id, target_kind, target_id)
             VALUES (?1, ?2, 'episode', ?3)",
            params![content, space, id],
        )
        .map_err(sql_err)?;
        conn.execute(
            "INSERT INTO fts_ngram (body, space_id, target_kind, target_id)
             VALUES (?1, ?2, 'episode', ?3)",
            params![content, space, id],
        )
        .map_err(sql_err)?;
    }
    // Index statement renderings.
    let statements = load_statements(conn, space)?;
    for stmt in &statements {
        let body = render_statement(conn, stmt)?;
        conn.execute(
            "INSERT INTO fts_word (body, space_id, target_kind, target_id)
             VALUES (?1, ?2, 'statement', ?3)",
            params![body, space, stmt.id],
        )
        .map_err(sql_err)?;
        conn.execute(
            "INSERT INTO fts_ngram (body, space_id, target_kind, target_id)
             VALUES (?1, ?2, 'statement', ?3)",
            params![body, space, stmt.id],
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
/// Format query rows as pipe-delimited lines for snapshot comparison.
/// Column count is discovered at runtime so each query may return any arity.
fn snapshot_query(
    conn: &Connection,
    label: &str,
    sql: &str,
    space: &str,
) -> Result<String, BrainError> {
    snapshot_query_params(conn, label, sql, params![space])
}

/// Same as `snapshot_query` but for global queries with no space parameter.
fn snapshot_query_global(conn: &Connection, label: &str, sql: &str) -> Result<String, BrainError> {
    snapshot_query_params(conn, label, sql, [])
}

fn snapshot_query_params(
    conn: &Connection,
    label: &str,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<String, BrainError> {
    let mut stmt = conn.prepare(sql).map_err(sql_err)?;
    let n = stmt.column_count();
    let rows: Vec<String> = stmt
        .query_map(args, |r| {
            let mut parts = Vec::with_capacity(n);
            for i in 0..n {
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
    let mut out = format!("---{label}---\n");
    for r in rows {
        out.push_str(&r);
        out.push('\n');
    }
    Ok(out)
}

/// Byte-identical snapshot of the **truth half** of the projection (P1, §5.1).
///
/// Covers entities, entity_keys, entity_merges, statements, assertions,
/// mentions, beliefs, predicates. Must **not** include vectors, FTS, salience,
/// or any ranking-half data (F18). Same ledger → same string, byte for byte.
pub fn snapshot_truth(conn: &Connection, space: &str) -> Result<String, BrainError> {
    let mut out = String::new();
    // NOTE: salience / last_activity columns on entities are ranking-half —
    // excluded from this snapshot (P1, §5.1).
    for (label, sql) in [
        (
            "entities",
            "SELECT id, type_name, canonical_key, merged_into FROM entities WHERE space_id = ?1 ORDER BY id",
        ),
        (
            "keys",
            "SELECT id, entity_id, type_name, normalized, surface, origin FROM entity_keys WHERE space_id = ?1 ORDER BY id",
        ),
        (
            "merges",
            "SELECT id, loser_id, winner_id, decided_by, score, provenance, decided_at, undone_at FROM entity_merges WHERE loser_id IN (SELECT id FROM entities WHERE space_id = ?1) ORDER BY id",
        ),
        (
            "statements",
            "SELECT id, subject_id, predicate, object_entity, object_literal FROM statements WHERE space_id = ?1 ORDER BY id",
        ),
        (
            "assertions",
            "SELECT id, statement_id, episode_id, extractor_id, polarity, claimed_from, claimed_to, confidence, recorded_at, retracted_at FROM assertions WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1) ORDER BY id",
        ),
        (
            "mentions",
            "SELECT id, assertion_id, role, surface, span_start, span_end, resolved_to, method FROM mentions WHERE assertion_id IN (SELECT id FROM assertions WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1)) ORDER BY id",
        ),
        (
            "beliefs",
            "SELECT statement_id, valid_from, valid_to, status, confidence, support_json FROM beliefs WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1) ORDER BY statement_id, valid_from",
        ),
    ] {
        out.push_str(&snapshot_query(conn, label, sql, space)?);
    }
    // Global predicates table — no space filter.
    out.push_str(&snapshot_query_global(
        conn,
        "predicates",
        "SELECT name, major_version, minor_version, def_json FROM predicates ORDER BY name",
    )?);
    Ok(out)
}

/// Ranking-half snapshot: membership of derived indexes (P1, §5.1).
///
/// Equivalent contract: identical membership and retrieval recall within a
/// stated tolerance across rebuilds. Currently deterministic (no float
/// embeddings yet). When dense embeddings land (7.3/7.7), the tolerance is
/// calibrated and recorded in ARCHITECTURE.md §5.1.
pub fn snapshot_ranking(conn: &Connection, space: &str) -> Result<String, BrainError> {
    let mut out = String::new();
    for (label, sql) in [
        (
            "fts_word",
            "SELECT target_kind, target_id, body FROM fts_word WHERE space_id = ?1 ORDER BY target_kind, target_id",
        ),
        (
            "fts_ngram",
            "SELECT target_kind, target_id, body FROM fts_ngram WHERE space_id = ?1 ORDER BY target_kind, target_id",
        ),
        (
            "vectors",
            "SELECT target_kind, target_id, hex(vector) FROM tfidf_vectors WHERE space_id = ?1 ORDER BY target_kind, target_id",
        ),
        (
            "communities",
            "SELECT id, label FROM communities WHERE space_id = ?1 ORDER BY id",
        ),
        (
            "salience",
            "SELECT id, salience, last_activity FROM entities WHERE space_id = ?1 ORDER BY id",
        ),
    ] {
        out.push_str(&snapshot_query(conn, label, sql, space)?);
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
