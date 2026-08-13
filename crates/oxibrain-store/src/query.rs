//! Read queries: beliefs for an entity, as-of queries, contradictions, plus
//! lexical (FTS5), semantic (TF-IDF kNN), and hybrid (RRF) search.

use crate::knowledge as kcrud;
use crate::sql_err;
use oxibrain_core::knowledge::Object;

use oxibrain_core::rank::{
    Channel as RankChannel, ChannelResult, LexIndex, RankedItem, RankingResult, Rerank,
    Retrieval, RetrievalInput, TargetFacts, TargetId, VecSpace,
};
use oxibrain_core::retrieval::{
    Query, QueryMode, SearchHit, SearchTarget, TraversalEdge, TraversalNode, TraversalResult,
    TraversalSpec,
};
use oxibrain_core::{Belief, Statement};
use oxibrain_index::adjacency::{AdjacencyGraph, BfsSpec};
use oxibrain_index::rrf;
use oxibrain_index::{KnnIndex, TfIdfModel, TfIdfVector};

use oxibrain_ports::{BrainError, EmbeddingPort, Timestamp};
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

/// Aggregate counts for a space: episodes, entities (excluding merged-away),
/// statements, and contradicted statements.
pub fn space_stats(
    conn: &Connection,
    space: &str,
) -> Result<oxibrain_core::SpaceStats, BrainError> {
    let episodes = count(conn, "episodes", "space_id = ?1", space)?;
    let entities = count(
        conn,
        "entities",
        "space_id = ?1 AND merged_into IS NULL",
        space,
    )?;
    let statements = count(conn, "statements", "space_id = ?1", space)?;
    let contradictions = contradictions(conn, space)?.len();
    Ok(oxibrain_core::SpaceStats {
        episodes,
        entities,
        statements,
        contradictions,
    })
}

/// Count rows in `table` matching `where_clause` (bound with a single space param).
fn count(
    conn: &Connection,
    table: &str,
    where_clause: &str,
    space: &str,
) -> Result<i64, BrainError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {where_clause}");
    conn.query_row(&sql, params![space], |r| r.get(0))
        .map_err(sql_err)
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

/// Which FTS5 index to search (§7.4).
#[derive(Clone, Copy)]
pub enum FtsIndex {
    Word,
    Ngram,
}

/// FTS5/BM25 lexical search over one index. Returns hits sorted by BM25 score
/// descending (FTS5 rank is negated so higher = better). Call twice — once per
/// index — so both lists enter RRF as separate channels (§7.4).
pub fn fts_search(
    conn: &Connection,
    space: &str,
    query_text: &str,
    limit: usize,
    index: FtsIndex,
) -> Result<Vec<SearchHit>, BrainError> {
    let table = match index {
        FtsIndex::Word => "fts_word",
        FtsIndex::Ngram => "fts_ngram",
    };
    // FTS5 implicit-AND query: space-separated tokens. Each token is quoted
    // so FTS5 treats it as a literal phrase — punctuation in the query
    // (`?`, `(`, `:`, `*`, `-`, …) cannot break the MATCH expression with a
    // syntax error. A literal `"` inside a token is escaped by doubling.
    let fts_query: String = query_text
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT target_kind, target_id, rank
         FROM {table}
         WHERE {table} MATCH ?1 AND space_id = ?2
         ORDER BY rank
         LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
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

/// Lexical-vector search: prefer sqlite-vec KNN over dense entity_vectors; fall
/// back to TF-IDF brute-force kNN if no vectors exist (v1 default — no model
/// loaded). This is a *lexical* channel (hashed bag-of-shingles), not semantic —
/// the name was corrected because calling it "semantic" is how F16 survived
/// review (§7.3, §7.4).
pub fn lexical_vector_search(
    conn: &Connection,
    space: &str,
    query_text: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, BrainError> {
    // Lexical-vector channel: n-gram shingles hashed into a fixed-dim vector,
    // searched via the TF-IDF kNN index. This is the language-independent
    // fallback (§7.3); the DENSE embedding channel is a separate path
    // (QueryMode::Dense) that requires an EmbeddingPort.
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
                mode: QueryMode::LexicalVector,
            }
        })
        .collect();
    Ok(hits)
}

/// Dense embedding search: embed the query, KNN via sqlite-vec (§7.6, F16).
///
/// Requires a configured [`EmbeddingPort`]. Returns an explicit error when
/// the embedder is absent — never a silent lexical substitute.
pub fn dense_search(
    conn: &Connection,
    embedder: &dyn EmbeddingPort,
    query_text: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, BrainError> {
    let vecs = embedder
        .embed(&[query_text])
        .map_err(|e| BrainError::Config(format!("query embedding: {e}")))?;
    let query_vec = vecs
        .first()
        .ok_or_else(|| BrainError::Config("embedder returned no vectors".into()))?;
    if query_vec.len() != crate::vectors::EMBEDDING_DIM {
        return Err(BrainError::Config(format!(
            "embedder dim {} != table dim {}",
            query_vec.len(),
            crate::vectors::EMBEDDING_DIM
        )));
    }
    let hits = crate::vectors::knn_search(conn, query_vec, limit)?;
    Ok(hits
        .into_iter()
        .map(|h| SearchHit {
            target: SearchTarget::Entity { id: h.entity_id },
            score: 1.0 - h.distance,
            mode: QueryMode::Dense,
        })
        .collect())
}

/// Hybrid (or mode-specific) query: run the matching modes, fuse their result
/// lists via `core::rank` (DESIGN §11.3), and emit a [`RankingResult`] with
/// provenance.
///
/// `embedder` is required for `QueryMode::Dense` (and the dense channel of
/// `Hybrid`). When a dense channel is requested without an embedder, this
/// returns an explicit error — never a silent lexical substitute (§7.6, F16).
pub fn hybrid_query(
    conn: &Connection,
    q: &Query,
    embedder: Option<&dyn EmbeddingPort>,
) -> Result<RankingResult, BrainError> {
    let limit = q.limit;
    let space = q.space.clone();

    // 1. Translate the legacy QueryMode into an M8 Retrieval spec.
    let mut spec = match q.mode {
        QueryMode::Hybrid => Retrieval::hybrid(&space),
        QueryMode::Lexical => Retrieval::lexical(&space),
        QueryMode::LexicalVector => Retrieval::lexical(&space),
        QueryMode::Dense => Retrieval::semantic(&space),
        QueryMode::Graph => {
            let mut s = Retrieval::graph(&space, Vec::new());
            s.filters.min_confidence = q.min_confidence;
            s
        }
        QueryMode::Community => Retrieval::community(&space, Vec::new()),
    };
    if let Some(t) = q.as_of {
        spec.filters.as_of = Some(t);
    }
    if q.min_confidence > 0.0 {
        spec.filters.min_confidence = q.min_confidence;
    }
    spec.limit = limit;

    // 2. Execute channels into ChannelResult form. Each channel gets a stable
    //    positional index that matches `spec.channels`. Embedder errors here
    //    keep the M7 contract: explicit Dense without an embedder fails.
    let mut input = RetrievalInput::default();
    let mut next_channel: u8 = 0;
    let run_lexical =
        matches!(q.mode, QueryMode::Hybrid | QueryMode::Lexical | QueryMode::LexicalVector);
    let run_dense = matches!(q.mode, QueryMode::Dense)
        || (matches!(q.mode, QueryMode::Hybrid) && embedder.is_some());
    let run_graph = matches!(q.mode, QueryMode::Hybrid | QueryMode::Graph);
    let run_community = matches!(q.mode, QueryMode::Hybrid | QueryMode::Community);

    let mut channels_used: Vec<RankChannel> = Vec::new();

    if run_lexical {
        let word = fts_search(conn, &space, &q.text, limit, FtsIndex::Word)?;
        let ngram = fts_search(conn, &space, &q.text, limit, FtsIndex::Ngram)?;
        input.channels.push(ChannelResult {
            channel: next_channel,
            hits: word.iter().map(|h| (search_target_to_target_id(&h.target), h.score)).collect(),
        });
        channels_used.push(RankChannel::Lexical { index: LexIndex::Word });
        next_channel += 1;
        input.channels.push(ChannelResult {
            channel: next_channel,
            hits: ngram.iter().map(|h| (search_target_to_target_id(&h.target), h.score)).collect(),
        });
        channels_used.push(RankChannel::Lexical { index: LexIndex::Ngram });
        next_channel += 1;
    }
    if matches!(q.mode, QueryMode::Hybrid | QueryMode::LexicalVector) {
        let hits = lexical_vector_search(conn, &space, &q.text, limit)?;
        input.channels.push(ChannelResult {
            channel: next_channel,
            hits: hits.iter().map(|h| (search_target_to_target_id(&h.target), h.score)).collect(),
        });
        channels_used.push(RankChannel::Vector { space: VecSpace::Entity });
        next_channel += 1;
    }
    if run_dense {
        let embedder = embedder.ok_or_else(|| {
            BrainError::Config(
                "QueryMode::Dense requires a configured embedder; none is available \
                 (run `oxibrain model pull` and configure the embed port)"
                    .into(),
            )
        })?;
        let hits = dense_search(conn, embedder, &q.text, limit)?;
        input.channels.push(ChannelResult {
            channel: next_channel,
            hits: hits.iter().map(|h| (search_target_to_target_id(&h.target), h.score)).collect(),
        });
        channels_used.push(RankChannel::Vector { space: VecSpace::Entity });
        next_channel += 1;
    }
    if run_graph {
        // Graph mode: seed from lexical entity hits, BFS expand to neighbors.
        let seed_hits = fts_search(conn, &space, &q.text, limit / 2, FtsIndex::Word)?
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
            let graph = load_adjacency(conn, &space)?;
            let bfs_spec = BfsSpec {
                start: seeds.clone(),
                max_depth: 2,
                max_nodes: (limit * 2) as u32,
                direction: oxibrain_core::retrieval::Direction::Both,
                predicate_filter: oxibrain_core::retrieval::PredicateFilter::AllowAll,
            };
            let bfs_result = graph.bfs(&bfs_spec);
            let seed_set: std::collections::HashSet<&str> =
                seeds.iter().map(|s| s.as_str()).collect();
            let mut graph_hits: Vec<SearchHit> = bfs_result
                .nodes
                .keys()
                .filter(|id| !seed_set.contains(id.as_str()))
                .map(|id| SearchHit {
                    target: SearchTarget::Entity { id: id.clone() },
                    score: 0.5,
                    mode: QueryMode::Graph,
                })
                .collect();
            for mut h in seed_hits {
                h.mode = QueryMode::Graph;
                graph_hits.push(h);
            }
            input.channels.push(ChannelResult {
                channel: next_channel,
                hits: graph_hits
                    .iter()
                    .map(|h| (search_target_to_target_id(&h.target), h.score))
                    .collect(),
            });
            channels_used.push(RankChannel::GraphExpand {
                seed: oxibrain_core::rank::SeedPolicy::FromHits { top_k: 5 },
                depth: 2,
            });
            next_channel += 1;
        }
    }
    if run_community {
        let seed_hits = fts_search(conn, &space, &q.text, limit / 2, FtsIndex::Word)?
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
                if let Ok(members) = crate::communities::community_members(conn, &space, seed_id) {
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
            for mut h in seed_hits {
                h.mode = QueryMode::Community;
                community_hits.push(h);
            }
            input.channels.push(ChannelResult {
                channel: next_channel,
                hits: community_hits
                    .iter()
                    .map(|h| (search_target_to_target_id(&h.target), h.score))
                    .collect(),
            });
            channels_used.push(RankChannel::CommunityExpand {
                seed: oxibrain_core::rank::SeedPolicy::FromHits { top_k: 5 },
            });
            next_channel += 1;
        }
    }

    // 3. Restrict the spec to channels actually executed, then batch-fetch
    //    facts and run rank. Folding facts happens here so the spec's
    //    Filters (as_of, known_at, min_confidence) can be applied by `rank`.
    spec.channels = channels_used;
    fetch_facts_for_candidates(conn, &space, &mut input, q.as_of, q.min_confidence)?;
    Ok(oxibrain_core::rank::rank(&input, &spec))
}

/// Translate the legacy `SearchTarget` to the new `TargetId`.
fn search_target_to_target_id(t: &SearchTarget) -> TargetId {
    match t {
        SearchTarget::Episode { id } => TargetId::Episode { id: id.clone() },
        SearchTarget::Statement { id } => TargetId::Statement { id: id.clone() },
        SearchTarget::Entity { id } => TargetId::Entity { id: id.clone() },
    }
}

/// Batch-fetch `TargetFacts` for every candidate the channels emitted. Joins
/// statements + beliefs (current slice) and pulls confidence, validity,
/// recorded_at, retracted_at, status, trust, and salience. Single round-trip
/// per kind — this is the "ONE TargetFacts query" §11.3 promises.
fn fetch_facts_for_candidates(
    conn: &Connection,
    space: &str,
    input: &mut RetrievalInput,
    as_of: Option<Timestamp>,
    min_confidence: f32,
) -> Result<(), BrainError> {
    // Collect candidate target ids by kind.
    let mut statements: Vec<String> = Vec::new();
    let mut entities: Vec<String> = Vec::new();
    let mut episodes: Vec<String> = Vec::new();
    for cr in &input.channels {
        for (t, _) in &cr.hits {
            match t {
                TargetId::Statement { id } => statements.push(id.clone()),
                TargetId::Entity { id } => entities.push(id.clone()),
                TargetId::Episode { id } => episodes.push(id.clone()),
                _ => {}
            }
        }
    }
    statements.sort();
    statements.dedup();
    entities.sort();
    entities.dedup();
    episodes.sort();
    episodes.dedup();

    // 1. Statements + beliefs — the join that yields confidence, validity,
    //    status, retracted_at, and predicate. For each statement we pick the
    //    current belief row (valid_from desc, LIMIT 1).
    if !statements.is_empty() {
        let placeholders = statements.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT s.id, s.predicate, b.confidence, b.valid_from, b.valid_to,
                    b.status, a.recorded_at, a.retracted_at, s.subject_id, s.space_id
             FROM statements s
             LEFT JOIN beliefs b ON b.statement_id = s.id
             LEFT JOIN (
                 SELECT statement_id, MAX(recorded_at) AS recorded_at,
                        MAX(retracted_at) AS retracted_at
                 FROM assertions
                 GROUP BY statement_id
             ) a ON a.statement_id = s.id
             WHERE s.space_id = ? AND s.id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::new();
        params_vec.push(&space as &dyn rusqlite::ToSql);
        for id in &statements {
            params_vec.push(id as &dyn rusqlite::ToSql);
        }
        // The SQL we built uses '?' for the space id *first* then for each id.
        // params above match that. But the format string has `?` *in* the
        // placeholder list (e.g. "?,?,?"), so the order is: space, then ids.
        // Re-bind correctly: clear and re-push in canonical order.
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
                let stmt_id: String = r.get(0)?;
                let predicate: String = r.get(1)?;
                let confidence: Option<f64> = r.get(2)?;
                let valid_from: Option<i64> = r.get(3)?;
                let valid_to: Option<i64> = r.get(4)?;
                let status: Option<String> = r.get(5)?;
                let recorded_at: Option<i64> = r.get(6)?;
                let retracted_at: Option<i64> = r.get(7)?;
                let subject: Option<String> = r.get(8)?;
                let row_space: Option<String> = r.get(9)?;
                Ok((
                    stmt_id,
                    predicate,
                    confidence.unwrap_or(0.0) as f32,
                    valid_from.unwrap_or(oxibrain_ports::TIME_MIN.0),
                    valid_to.unwrap_or(oxibrain_ports::TIME_MAX.0),
                    status.unwrap_or_else(|| "active".into()),
                    recorded_at.unwrap_or(0),
                    retracted_at,
                    subject.unwrap_or_default(),
                    row_space.unwrap_or_default(),
                ))
            })
            .map_err(sql_err)?;
        let salience_lookup = fetch_salience(conn, space, &entities)?;
        for row in rows {
            let (stmt_id, predicate, confidence, vf, vt, status, recorded_at, retracted_at, _subject, _row_space) = row.map_err(sql_err)?;
            let trust = oxibrain_core::TrustTier::Trusted;
            let salience = *salience_lookup.get(&stmt_id).unwrap_or(&0.5);
            let _ = min_confidence; // already applied via spec.filters
            let _ = as_of;          // already applied via spec.filters
            let target = TargetId::Statement { id: stmt_id.clone() };
            input.facts.insert(
                target,
                TargetFacts {
                    target: TargetId::Statement { id: stmt_id },
                    confidence,
                    valid_from: oxibrain_ports::Timestamp(vf),
                    valid_to: oxibrain_ports::Timestamp(vt),
                    recorded_at: oxibrain_ports::Timestamp(recorded_at),
                    retracted_at: retracted_at.map(oxibrain_ports::Timestamp),
                    trust,
                    status: oxibrain_core::BeliefStatus::parse_db(&status)
                        .unwrap_or(oxibrain_core::BeliefStatus::Active),
                    predicate,
                    salience,
                    distinct_episodes: 1,
                    channels: Vec::new(),
                    channel_scores: Vec::new(),
                },
            );
        }
    }

    // 2. Entities — pull salience from entities.salience. Other fields are
    //    unknown for non-statement targets; `rank` treats them as `Active`
    //    with sentinel validity so as_of/known_at don't drop them.
    if !entities.is_empty() {
        let placeholders = entities.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, salience FROM entities WHERE space_id = ? AND id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::new();
        params_vec.push(&space as &dyn rusqlite::ToSql);
        for id in &entities {
            params_vec.push(id as &dyn rusqlite::ToSql);
        }
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (id, salience) = row.map_err(sql_err)?;
            let target = TargetId::Entity { id: id.clone() };
            input.facts.entry(target.clone()).or_insert_with(|| TargetFacts {
                target,
                confidence: 1.0,
                valid_from: oxibrain_ports::TIME_MIN,
                valid_to: oxibrain_ports::TIME_MAX,
                recorded_at: oxibrain_ports::TIME_MIN,
                retracted_at: None,
                trust: oxibrain_core::TrustTier::Trusted,
                status: oxibrain_core::BeliefStatus::Active,
                predicate: String::new(),
                salience,
                distinct_episodes: 0,
                channels: Vec::new(),
                channel_scores: Vec::new(),
            });
        }
    }

    // 3. Episodes — minimal facts; episode-level retrieval is rare and
    //    doesn't go through the fold.
    for id in &episodes {
        let target = TargetId::Episode { id: id.clone() };
        input.facts.entry(target.clone()).or_insert_with(|| TargetFacts {
            target,
            confidence: 1.0,
            valid_from: oxibrain_ports::TIME_MIN,
            valid_to: oxibrain_ports::TIME_MAX,
            recorded_at: oxibrain_ports::TIME_MIN,
            retracted_at: None,
            trust: oxibrain_core::TrustTier::Trusted,
            status: oxibrain_core::BeliefStatus::Active,
            predicate: String::new(),
            salience: 1.0,
            distinct_episodes: 0,
            channels: Vec::new(),
            channel_scores: Vec::new(),
        });
    }
    Ok(())
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
