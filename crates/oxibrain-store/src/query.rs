//! Read queries: beliefs for an entity, as-of queries, contradictions, plus
//! lexical (FTS5), semantic (TF-IDF kNN), and hybrid (RRF) search.

use crate::knowledge as kcrud;
use crate::sql_err;
use oxibrain_core::knowledge::Object;

use oxibrain_core::retrieval::{QueryMode, SearchHit, SearchTarget};
use oxibrain_core::{Belief, Statement};
use oxibrain_index::{KnnIndex, TfIdfModel, TfIdfVector};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

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

/// FTS5/BM25 lexical search over the indexed body texts. Returns hits
/// sorted by BM25 score descending (FTS5 rank is negated so higher = better).
pub fn fts_search(
    conn: &Connection,
    space: &str,
    query_text: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, BrainError> {
    // FTS5 implicit-AND query: space-separated tokens.
    let fts_query: String = query_text
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT target_kind, target_id, rank
             FROM episodes_fts
             WHERE episodes_fts MATCH ?1 AND space_id = ?2
             ORDER BY rank
             LIMIT ?3",
        )
        .map_err(sql_err)?;
    let hits = stmt
        .query_map(params![&fts_query, space, limit as i64], |r| {
            let kind: String = r.get(0)?;
            let id: String = r.get(1)?;
            let rank: f64 = r.get(2)?;
            let target = match kind.as_str() {
                "statement" => SearchTarget::Statement { id },
                "entity" => SearchTarget::Entity { id },
                _ => SearchTarget::Episode { id },
            };
            // FTS5 rank: lower = better. Negate so higher = better.
            Ok(SearchHit {
                target,
                score: -rank,
                mode: QueryMode::Lexical,
            })
        })
        .map_err(sql_err)?;
    let mut results = Vec::new();
    for hit in hits {
        results.push(hit.map_err(sql_err)?);
    }
    Ok(results)
}

/// Load the TF-IDF model for a space, fitted on the live (non-redacted)
/// episode contents. Statement rendering is kept out of the model so the
/// model is cheap to rebuild on each query.
pub fn load_tfidf_model(
    conn: &Connection,
    space: &str,
    dim: usize,
) -> Result<TfIdfModel, BrainError> {
    let mut stmt = conn
        .prepare("SELECT content FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL")
        .map_err(sql_err)?;
    let texts: Vec<String> = stmt
        .query_map(params![space], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    Ok(TfIdfModel::fit(&text_refs, dim))
}

/// Load all persisted TF-IDF vectors for a space into an in-memory KnnIndex.
pub fn load_knn_index(conn: &Connection, space: &str) -> Result<KnnIndex, BrainError> {
    let mut index = KnnIndex::new();
    let mut stmt = conn
        .prepare(
            "SELECT target_kind, target_id, vector FROM tfidf_vectors WHERE space_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            let kind: String = r.get(0)?;
            let id: String = r.get(1)?;
            let vector_blob: Vec<u8> = r.get(2)?;
            Ok((kind, id, vector_blob))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (kind, id, blob) = row.map_err(sql_err)?;
        let key = format!("{kind}:{id}");
        let vector = TfIdfVector::from_bytes(&blob);
        index.insert(key, vector);
    }
    Ok(index)
}

/// Semantic (TF-IDF kNN) search: transform the query into a TF-IDF vector and
/// rank all persisted vectors by cosine similarity.
pub fn semantic_search(
    conn: &Connection,
    space: &str,
    query_text: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, BrainError> {
    let model = load_tfidf_model(conn, space, 1024)?;
    let query_vec = model.transform(query_text);
    let index = load_knn_index(conn, space)?;
    let results = index.search(&query_vec, limit);
    let hits = results
        .into_iter()
        .map(|(key, score)| {
            let (kind, id) = key.split_once(':').unwrap_or(("episode", &key));
            let target = match kind {
                "statement" => SearchTarget::Statement { id: id.to_string() },
                "entity" => SearchTarget::Entity { id: id.to_string() },
                _ => SearchTarget::Episode { id: id.to_string() },
            };
            SearchHit {
                target,
                score,
                mode: QueryMode::Semantic,
            }
        })
        .collect();
    Ok(hits)
}
