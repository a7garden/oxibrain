//! Index orchestration: FTS5 population, TF-IDF model build, vector persistence.
//! All operations are deterministic functions of the projection data.

use crate::sql_err;
use oxibrain_core::chunk_id;
use oxibrain_core::chunking::{ChunkPolicy, render_context_prefix, split_into_chunks};
use oxibrain_core::knowledge::{Object, Statement};
use oxibrain_core::object_repr;
#[allow(unused_imports)]
use oxibrain_index::{TfIdfModel, TfIdfVector, features};
use oxibrain_ports::{BrainError, EmbeddingPort, Timestamp};
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

pub fn entity_surface(conn: &Connection, entity_id: &str) -> Result<String, BrainError> {
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
/// Incrementally index a single episode body into both FTS tables.
///
/// Keeps the lexical index in sync at ingest time — the full
/// [`rebuild_fts`] pass only runs on `rebuild_indexes`/`reextract`, which
/// would otherwise leave freshly ingested episodes unsearchable until then.
pub fn index_episode_fts(
    conn: &Connection,
    space: &str,
    episode_id: &str,
    content: &str,
) -> Result<(), BrainError> {
    conn.execute(
        "INSERT INTO fts_word (body, space_id, target_kind, target_id)
         VALUES (?1, ?2, 'episode', ?3)",
        params![content, space, episode_id],
    )
    .map_err(sql_err)?;
    conn.execute(
        "INSERT INTO fts_ngram (body, space_id, target_kind, target_id)
         VALUES (?1, ?2, 'episode', ?3)",
        params![content, space, episode_id],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Incrementally refresh the entity-surface rows of both FTS indexes for the
/// given entity ids — the per-declaration counterpart of the entity pass in
/// [`rebuild_fts`]. Declarations create entities outside the ingest path, so
/// without this hook freshly declared entities stay unsearchable until a
/// full `rebuild_indexes`. Idempotent: each id's rows are deleted before
/// re-inserting every `entity_keys` surface, so the content always equals
/// what a rebuild would produce (P1: the ranking half stays a pure
/// projection of `entity_keys`).
pub fn index_entities_fts(
    conn: &Connection,
    space: &str,
    entity_ids: &[String],
) -> Result<(), BrainError> {
    for id in entity_ids {
        conn.execute(
            "DELETE FROM fts_word
             WHERE space_id = ?1 AND target_kind = 'entity' AND target_id = ?2",
            params![space, id],
        )
        .map_err(sql_err)?;
        conn.execute(
            "DELETE FROM fts_ngram
             WHERE space_id = ?1 AND target_kind = 'entity' AND target_id = ?2",
            params![space, id],
        )
        .map_err(sql_err)?;
        let mut stmt = conn
            .prepare(
                "SELECT surface FROM entity_keys
                 WHERE space_id = ?1 AND entity_id = ?2",
            )
            .map_err(sql_err)?;
        let surfaces: Vec<String> = stmt
            .query_map(params![space, id], |r| r.get::<_, String>(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        drop(stmt);
        for surface in &surfaces {
            conn.execute(
                "INSERT INTO fts_word (body, space_id, target_kind, target_id)
                 VALUES (?1, ?2, 'entity', ?3)",
                params![surface, space, id],
            )
            .map_err(sql_err)?;
            conn.execute(
                "INSERT INTO fts_ngram (body, space_id, target_kind, target_id)
                 VALUES (?1, ?2, 'entity', ?3)",
                params![surface, space, id],
            )
            .map_err(sql_err)?;
        }
    }
    Ok(())
}

pub fn rebuild_fts(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute("DELETE FROM fts_word WHERE space_id = ?1", params![space])
        .map_err(sql_err)?;
    conn.execute("DELETE FROM fts_ngram WHERE space_id = ?1", params![space])
        .map_err(sql_err)?;
    // Index episodes.
    // Declaration episodes carry machine-readable canonical JSON, not
    // human text — they must not pollute the retrieval index. Only
    // primary episodes (real source text) are searchable here.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
              WHERE space_id = ?1 AND redacted_at IS NULL AND kind != 'declaration'",
        )
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
    // Index entity surfaces so that FTS search by entity name returns Entity
    // targets — the graph expansion seeds from these (§11.4). Without this,
    // graph expansion finds no seeds and hybrid silently degrades to lexical.
    let mut entity_stmt = conn
        .prepare("SELECT entity_id, surface FROM entity_keys WHERE space_id = ?1")
        .map_err(sql_err)?;
    let entity_rows: Vec<(String, String)> = entity_stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(entity_stmt);
    for (eid, surface) in &entity_rows {
        conn.execute(
            "INSERT INTO fts_word (body, space_id, target_kind, target_id)
             VALUES (?1, ?2, 'entity', ?3)",
            params![surface, space, eid],
        )
        .map_err(sql_err)?;
        conn.execute(
            "INSERT INTO fts_ngram (body, space_id, target_kind, target_id)
             VALUES (?1, ?2, 'entity', ?3)",
            params![surface, space, eid],
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
        .prepare(
            "SELECT id, content FROM episodes
              WHERE space_id = ?1 AND redacted_at IS NULL AND kind != 'declaration'",
        )
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
        (
            "chunks",
            "SELECT episode_id, ordinal, span_start, span_end, context FROM chunks WHERE space_id = ?1 ORDER BY episode_id, ordinal",
        ),
    ] {
        out.push_str(&snapshot_query(conn, label, sql, space)?);
    }
    Ok(out)
}

/// Rebuild the `chunks` table for a space (§5.7, §9.3, M8 §8.11).
///
/// Splits each non-redacted episode's content with the recursive splitter and
/// writes a deterministic context prefix (occurred_at · source · mentions).
/// Chunks are ranking-half derived state (§5.1): pure functions of projection
/// data, rebuilt on every [`rebuild_indexes`]/`reproject`. Chunk text is not
/// stored — it is recovered from `episodes.content` via the byte offsets.
pub fn rebuild_chunks(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute("DELETE FROM chunks WHERE space_id = ?1", params![space])
        .map_err(sql_err)?;

    // Entity surfaces mentioned in each episode's statements (verbatim mention
    // surfaces, deduplicated and ordered for determinism).
    let mut mention_stmt = conn
        .prepare(
            "SELECT DISTINCT a.episode_id, m.surface
               FROM mentions m
               JOIN assertions a ON m.assertion_id = a.id
               JOIN statements s ON a.statement_id = s.id
              WHERE s.space_id = ?1
              ORDER BY a.episode_id, m.surface",
        )
        .map_err(sql_err)?;
    let mention_rows = mention_stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<(String, String)>, _>>()
        .map_err(sql_err)?;
    drop(mention_stmt);

    let mut mentions: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (ep, surface) in mention_rows {
        mentions.entry(ep).or_default().push(surface);
    }

    // Read all non-redacted episodes in canonical (seq) order.
    let mut ep_stmt = conn
        .prepare(
            "SELECT id, content, occurred_at, source_kind
               FROM episodes
              WHERE space_id = ?1 AND redacted_at IS NULL
              ORDER BY seq ASC",
        )
        .map_err(sql_err)?;
    let episodes = ep_stmt
        .query_map(params![space], |r| {
            Ok((
                r.get::<_, String>(0)?, // id
                r.get::<_, String>(1)?, // content
                r.get::<_, i64>(2)?,    // occurred_at
                r.get::<_, String>(3)?, // source_kind
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<(String, String, i64, String)>, _>>()
        .map_err(sql_err)?;
    drop(ep_stmt);

    let policy = ChunkPolicy::default();
    let empty: Vec<String> = Vec::new();
    for (id, content, occurred_at, source_kind) in episodes {
        let episode_mentions = mentions.get(&id).unwrap_or(&empty);
        let chunks = split_into_chunks(&content, &policy);
        for chunk in chunks {
            let cid = chunk_id(&id, chunk.ordinal);
            let context =
                render_context_prefix(Timestamp(occurred_at), &source_kind, episode_mentions, None);
            conn.execute(
                "INSERT OR REPLACE INTO chunks
                   (id, space_id, episode_id, ordinal, span_start, span_end, context)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    cid,
                    space,
                    id,
                    chunk.ordinal as i64,
                    chunk.span_start as i64,
                    chunk.span_end as i64,
                    context,
                ],
            )
            .map_err(sql_err)?;
        }
    }

    Ok(())
}

pub fn rebuild_indexes(conn: &Connection, space: &str) -> Result<(), BrainError> {
    rebuild_fts(conn, space)?;
    rebuild_tfidf(conn, space, 1024)?;
    rebuild_salience(conn, space)?;
    rebuild_chunks(conn, space)?;
    Ok(())
}

/// Collect entity embedding texts for a space (§7.6, F17).
/// Returns (entity_id, embedding_text) pairs. Read-only — safe on a
/// reader connection.
pub fn entity_embedding_texts(
    conn: &Connection,
    space: &str,
) -> Result<Vec<(String, String)>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT e.id, e.type_name,
                    COALESCE(ek.surface, '') AS canonical_surface
             FROM entities e
             LEFT JOIN entity_keys ek ON ek.id = e.canonical_key
            WHERE e.space_id = ?1 AND e.merged_into IS NULL
            ORDER BY e.id",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(sql_err)?;
    let entities: Vec<(String, String, String)> =
        rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)?;
    drop(stmt);

    // Build alias lists per entity.
    let ids: Vec<&str> = entities.iter().map(|(id, _, _)| id.as_str()).collect();
    let mut alias_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    if !ids.is_empty() {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut alias_stmt = conn
            .prepare(&format!(
                "SELECT entity_id, surface FROM entity_keys
                 WHERE space_id = ?1 AND entity_id IN ({placeholders})"
            ))
            .map_err(sql_err)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&space as &dyn rusqlite::ToSql];
        for id in &ids {
            params_vec.push(id);
        }
        let alias_rows = alias_stmt
            .query_map(&params_vec[..], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        for row in alias_rows {
            let (eid, surface) = row.map_err(sql_err)?;
            alias_map.entry(eid).or_default().push(surface);
        }
    }

    // Build embedding text: "type: canonical alias other alias ..."
    let mut out = Vec::with_capacity(entities.len());
    for (id, type_name, canonical) in &entities {
        let mut t = format!("{type_name}: {canonical}");
        if let Some(extra) = alias_map.get(id) {
            for a in extra {
                if a != canonical {
                    t.push_str(" alias ");
                    t.push_str(a);
                }
            }
        }
        out.push((id.clone(), t));
    }
    Ok(out)
}

/// Upsert pre-computed entity embeddings (§7.6, F17). Requires a writable
/// connection; each upsert is an independent write (no single transaction).
pub fn upsert_entity_embeddings(
    conn: &Connection,
    items: &[(String, Vec<f32>)],
) -> Result<usize, BrainError> {
    for (entity_id, vec) in items {
        crate::vectors::upsert_vector(conn, entity_id, vec)?;
    }
    Ok(items.len())
}

/// Compute and upsert dense entity embeddings (§7.6, F17).
///
/// Convenience wrapper over [`entity_embedding_texts`] + [`upsert_entity_embeddings`]
/// for callers holding a writable connection. Embeddings run OUTSIDE any
/// transaction (P9: never embed inside a transaction). Returns the number of
/// entities embedded.
pub fn embed_entities(
    conn: &Connection,
    space: &str,
    embedder: &dyn EmbeddingPort,
) -> Result<usize, BrainError> {
    let items = entity_embedding_texts(conn, space)?;
    if items.is_empty() {
        return Ok(0);
    }
    let text_refs: Vec<&str> = items.iter().map(|(_, t)| t.as_str()).collect();
    let vectors = embedder
        .embed(&text_refs)
        .map_err(|e| BrainError::Config(format!("entity embedding: {e}")))?;
    let with_vectors: Vec<(String, Vec<f32>)> = items
        .into_iter()
        .zip(vectors)
        .map(|((id, _), v)| (id, v))
        .collect();
    upsert_entity_embeddings(conn, &with_vectors)
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
                (None, Some(lit)) => {
                    Object::Literal(serde_json::from_str(&lit).expect("valid literal in db"))
                }
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
