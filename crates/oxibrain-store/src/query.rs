//! Read queries: beliefs for an entity, as-of queries, contradictions, plus
//! lexical (FTS5), semantic (TF-IDF kNN), and hybrid (RRF) search.

use crate::knowledge as kcrud;
use crate::sql_err;
use oxibrain_core::knowledge::Object;

use oxibrain_core::retrieval::{
    DroppedItem, Query, QueryMode, RankedItem, RankingResult, SearchHit, SearchTarget,
    TraversalEdge, TraversalNode, TraversalResult, TraversalSpec,
};
use oxibrain_core::{Belief, Statement};
use oxibrain_index::adjacency::{AdjacencyGraph, BfsSpec};
use oxibrain_index::rrf;
use oxibrain_index::{KnnIndex, TfIdfModel, TfIdfVector};

use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

/// Batch-fetch salience values for a set of entity IDs.
/// Returns a map of entity_id → salience (default 1.0 if not found).
fn fetch_salience(
    conn: &Connection,
    space: &str,
    entity_ids: &[String],
) -> Result<std::collections::HashMap<String, f64>, BrainError> {
    if entity_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = entity_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql =
        format!("SELECT id, salience FROM entities WHERE space_id = ? AND id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&space as &dyn rusqlite::ToSql];
    for id in entity_ids {
        params_vec.push(id);
    }
    let rows = stmt
        .query_map(&params_vec[..], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .map_err(sql_err)?;
    let mut map = std::collections::HashMap::new();
    for row in rows {
        let (id, salience) = row.map_err(sql_err)?;
        map.insert(id, salience);
    }
    Ok(map)
}

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
        .prepare("SELECT target_kind, target_id, vector FROM tfidf_vectors WHERE space_id = ?1")
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

/// Convert a SearchHit to a stable "kind:id" RRF key.
fn hit_key(hit: &SearchHit) -> String {
    match &hit.target {
        SearchTarget::Episode { id } => format!("episode:{id}"),
        SearchTarget::Statement { id } => format!("statement:{id}"),
        SearchTarget::Entity { id } => format!("entity:{id}"),
    }
}

/// Parse an RRF key back into a SearchTarget. Unknown shapes fall back to
/// Episode so the result is never lossy.
fn parse_key(key: &str) -> SearchTarget {
    match key.split_once(':') {
        Some(("statement", id)) => SearchTarget::Statement { id: id.to_string() },
        Some(("entity", id)) => SearchTarget::Entity { id: id.to_string() },
        Some((_, id)) => SearchTarget::Episode { id: id.to_string() },
        None => SearchTarget::Episode {
            id: key.to_string(),
        },
    }
}

/// Hybrid (or mode-specific) query: run the matching modes, fuse their result
/// lists with RRF (`k=60`), and emit a [`RankingResult`] with provenance.
pub fn hybrid_query(conn: &Connection, q: &Query) -> Result<RankingResult, BrainError> {
    let limit = q.limit;
    let mut mode_lists: Vec<Vec<SearchHit>> = Vec::new();

    let run_lexical = matches!(q.mode, QueryMode::Hybrid | QueryMode::Lexical);
    let run_semantic = matches!(q.mode, QueryMode::Hybrid | QueryMode::Semantic);
    let run_graph = matches!(q.mode, QueryMode::Hybrid | QueryMode::Graph);
    let run_community = matches!(q.mode, QueryMode::Hybrid | QueryMode::Community);

    let dropped: Vec<DroppedItem> = Vec::new();

    if run_lexical {
        let hits = fts_search(conn, &q.space, &q.text, limit)?;
        mode_lists.push(hits);
    }
    if run_semantic {
        let hits = semantic_search(conn, &q.space, &q.text, limit)?;
        mode_lists.push(hits);
    }
    if run_graph {
        // Graph mode: seed from lexical entity hits, then BFS-expand to neighbors.
        let seed_hits = fts_search(conn, &q.space, &q.text, limit / 2)?
            .into_iter()
            .filter(|h| matches!(h.target, SearchTarget::Entity { .. }))
            .collect::<Vec<_>>();
        let seeds: Vec<String> = seed_hits
            .iter()
            .filter_map(|h| match &h.target {
                SearchTarget::Entity { id } => Some(id.clone()),
                _ => None,
            })
            .collect();

        if !seeds.is_empty() {
            // BFS expand from seed entities.
            let graph = load_adjacency(conn, &q.space)?;
            let bfs_spec = BfsSpec {
                start: seeds.clone(),
                max_depth: 2,
                max_nodes: (limit * 2) as u32,
                direction: oxibrain_core::retrieval::Direction::Both,
                predicate_filter: oxibrain_core::retrieval::PredicateFilter::AllowAll,
            };
            let bfs_result = graph.bfs(&bfs_spec);

            // Convert BFS nodes to SearchHit entries (neighbors that aren't seeds).
            let seed_set: std::collections::HashSet<&str> =
                seeds.iter().map(|s| s.as_str()).collect();
            let graph_hits: Vec<SearchHit> = bfs_result
                .nodes
                .keys()
                .filter(|id| !seed_set.contains(id.as_str()))
                .map(|id| SearchHit {
                    target: SearchTarget::Entity { id: id.clone() },
                    score: 0.5, // graph-expanded hits get a moderate base score
                    mode: QueryMode::Graph,
                })
                .collect();

            // Include the seed hits too (re-tagged).
            let mut all_graph = seed_hits
                .into_iter()
                .map(|mut h| {
                    h.mode = QueryMode::Graph;
                    h
                })
                .collect::<Vec<_>>();
            all_graph.extend(graph_hits);
            if !all_graph.is_empty() {
                mode_lists.push(all_graph);
            }
        }
    }
    if run_community {
        // Community mode: seed from lexical hits, expand to community members.
        let seed_hits = fts_search(conn, &q.space, &q.text, limit / 2)?
            .into_iter()
            .filter(|h| matches!(h.target, SearchTarget::Entity { .. }))
            .collect::<Vec<_>>();
        let seeds: Vec<String> = seed_hits
            .iter()
            .filter_map(|h| match &h.target {
                SearchTarget::Entity { id } => Some(id.clone()),
                _ => None,
            })
            .collect();

        if !seeds.is_empty() {
            let mut seen: std::collections::HashSet<String> = seeds.iter().cloned().collect();
            let mut community_hits: Vec<SearchHit> = Vec::new();
            for seed_id in &seeds {
                if let Ok(members) = crate::communities::community_members(conn, &q.space, seed_id)
                {
                    for member_id in members {
                        if seen.insert(member_id.clone()) {
                            community_hits.push(SearchHit {
                                target: SearchTarget::Entity { id: member_id },
                                score: 0.4,
                                mode: QueryMode::Community,
                            });
                        }
                    }
                }
            }
            // Include seed hits too (re-tagged).
            let mut all_community = seed_hits
                .into_iter()
                .map(|mut h| {
                    h.mode = QueryMode::Community;
                    h
                })
                .collect::<Vec<_>>();
            all_community.extend(community_hits);
            if !all_community.is_empty() {
                mode_lists.push(all_community);
            }
        }
    }

    // RRF expects `(key, raw_score)` tuples; the raw score is unused for
    // ranking (RRF is rank-based) but is preserved for downstream debugging.
    let rrf_lists: Vec<Vec<(String, f64)>> = mode_lists
        .iter()
        .map(|hits| hits.iter().map(|h| (hit_key(h), h.score)).collect())
        .collect();

    let fused = rrf::fuse(&rrf_lists, 60);

    // Batch-fetch salience for entity targets.
    let entity_ids: Vec<String> = fused
        .iter()
        .filter_map(|item| item.key.strip_prefix("entity:").map(String::from))
        .collect();
    let salience_map = fetch_salience(conn, &q.space, &entity_ids)?;

    let total_found = fused.len();
    let items: Vec<RankedItem> = fused
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(rank, item)| {
            let mode_ranks: Vec<(QueryMode, usize)> = mode_lists
                .iter()
                .filter_map(|hits| {
                    let pos = hits.iter().position(|h| hit_key(h) == item.key)?;
                    let mode = hits.first().map(|h| h.mode).unwrap_or(QueryMode::Lexical);
                    Some((mode, pos))
                })
                .collect();
            let target = parse_key(&item.key);
            let salience = match &target {
                SearchTarget::Entity { id } => *salience_map.get(id).unwrap_or(&1.0),
                _ => 1.0,
            };
            RankedItem {
                target,
                fused_score: item.score,
                rank,
                mode_ranks,
                salience,
            }
        })
        .collect();

    Ok(RankingResult {
        items,
        dropped,
        total_found,
        query: q.clone(),
    })
}

/// Load the entity→entity adjacency graph for a space from the statements table.
/// Only entity-typed objects produce edges; literal-valued statements are skipped.
pub fn load_adjacency(conn: &Connection, space: &str) -> Result<AdjacencyGraph, BrainError> {
    let mut graph = AdjacencyGraph::new();
    let mut stmt = conn
        .prepare(
            "SELECT subject_id, object_entity, predicate, id
             FROM statements
             WHERE space_id = ?1 AND object_entity IS NOT NULL",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (subj, obj, pred, id) = row.map_err(sql_err)?;
        graph.add_edge(&subj, &obj, &pred, &id);
    }
    Ok(graph)
}

/// Bounded BFS traversal over the statement-derived adjacency graph.
/// Honors the spec's `max_depth`, `max_nodes`, `direction`, and predicate filter.
pub fn traverse(
    conn: &Connection,
    space: &str,
    spec: &TraversalSpec,
) -> Result<TraversalResult, BrainError> {
    let graph = load_adjacency(conn, space)?;
    let bfs_spec = BfsSpec {
        start: spec.start.clone(),
        max_depth: spec.max_depth,
        max_nodes: spec.max_nodes,
        direction: spec.direction,
        predicate_filter: spec.predicates.clone(),
    };
    let bfs_result = graph.bfs(&bfs_spec);

    // Batch-fetch salience for all traversal nodes.
    let entity_ids: Vec<String> = bfs_result.nodes.keys().cloned().collect();
    let salience_map = fetch_salience(conn, space, &entity_ids)?;

    let nodes: Vec<TraversalNode> = bfs_result
        .nodes
        .iter()
        .map(|(entity, &depth)| TraversalNode {
            entity: entity.clone(),
            depth,
            salience: *salience_map.get(entity).unwrap_or(&1.0),
        })
        .collect();
    let edges: Vec<TraversalEdge> = bfs_result
        .edges
        .into_iter()
        .map(|(from, to, predicate, statement_id, depth)| TraversalEdge {
            from,
            to,
            predicate,
            statement_id,
            depth,
        })
        .collect();

    Ok(TraversalResult {
        nodes,
        edges,
        truncated: bfs_result.truncated,
    })
}

/// Look up a previously-resolved entity_id by surface form + type, returning
/// `None` if not yet declared. Cheap point query used by clients that need to
/// bootstrap a traversal start from a known surface.
pub fn resolve_entity_id(
    conn: &Connection,
    space: &str,
    ty: &str,
    surface: &str,
) -> Result<Option<String>, BrainError> {
    match conn.query_row(
        "SELECT entity_id FROM entity_keys
         WHERE space_id = ?1 AND type_name = ?2 AND surface = ?3
         LIMIT 1",
        params![space, ty, surface],
        |r| r.get::<_, String>(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(sql_err(e)),
    }
}

/// Extract all (predicate, subject_surface, object_surface) triples from a
/// space's current projection. Used by the eval suite and CLI `eval` command.
///
/// For entity objects, the surface comes from the object mention. For literal
/// objects, it is parsed from the statement's `object_literal` JSON `value`.
pub fn debug_triples(
    conn: &Connection,
    space: &str,
) -> Result<Vec<(String, String, String)>, BrainError> {
    let mut stmt_q = conn
        .prepare(
            "SELECT s.predicate, subj.surface, obj.surface, s.object_literal
             FROM assertions a
             JOIN statements s ON a.statement_id = s.id
             JOIN mentions subj ON subj.assertion_id = a.id AND subj.role = 'subject'
             LEFT JOIN mentions obj ON obj.assertion_id = a.id AND obj.role = 'object'
             WHERE s.space_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt_q
        .query_map(params![space], |r| {
            let predicate: String = r.get(0)?;
            let subject: String = r.get(1).unwrap_or_default();
            let object_mention: Option<String> = r.get(2).ok();
            let object_literal: Option<String> = r.get(3).ok();

            let object_surface = object_mention.unwrap_or_else(|| {
                object_literal
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                    .and_then(|v| v.get("value").and_then(|v| v.as_str()).map(String::from))
                    .unwrap_or_default()
            });
            Ok((predicate, subject, object_surface))
        })
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}
