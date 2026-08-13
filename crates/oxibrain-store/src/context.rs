//! Context assembly: packing policy for agent memory (DESIGN §9.5).

use crate::query;
use crate::sql_err;
use oxibrain_core::context::{ContextBudget, ContextLayer, ContextResult, LayerKind};
use oxibrain_core::retrieval::{Query, QueryMode, SearchTarget};
use oxibrain_ports::{BrainError, TokenizerPort};
use rusqlite::{Connection, params};

/// Hints that broaden context assembly (DESIGN §9.5, sub-project L3).
#[derive(Debug, Clone, Default)]
pub struct RecallHints {
    /// First query in this session — include community summaries + recent episodes.
    pub is_session_start: bool,
    /// Topic changed from prior query — widen neighborhood.
    pub topic_changed: bool,
    /// Recent query texts (for salience boost of entities mentioned repeatedly).
    pub recent_queries: Vec<String>,
}

/// Pack context for a query to a token budget. When `hints` is provided,
/// the layer composition adapts (DESIGN §9.5 proactive recall).
pub fn assemble_context(
    conn: &Connection,
    space: &str,
    query_text: &str,
    budget: usize,
    hints: Option<&RecallHints>,
    tokenizer: &dyn TokenizerPort,
) -> Result<ContextResult, BrainError> {
    let mut layers: Vec<ContextLayer> = Vec::new();
    let mut total_tokens = 0;
    let mut truncated = false;

    // Layer 1: Pinned facts (M2: empty — pin API is M4).

    // Layer 2: High-salience beliefs for query-relevant entities.
    let q = Query {
        text: query_text.to_string(),
        mode: QueryMode::Lexical,
        space: space.to_string(),
        as_of: None,
        limit: 10,
        min_confidence: 0.0,
    };
    let ranking = query::hybrid_query(conn, &q, None)?;
    let mut beliefs_text = String::new();
    let mut beliefs_provenance: Vec<String> = Vec::new();
    for item in &ranking.items {
        if let oxibrain_core::TargetId::Statement { id } = &item.target {
            let entry = render_belief(conn, space, id)?;
            beliefs_text.push_str(&entry.text);
            beliefs_text.push('\n');
            beliefs_provenance.push(id.clone());
        }
    }
    if !beliefs_text.is_empty() {
        let tokens = tokenizer.count(&beliefs_text);
        total_tokens += tokens;
        layers.push(ContextLayer {
            kind: LayerKind::HighSalienceBeliefs,
            text: beliefs_text,
            estimated_tokens: tokens,
            provenance: beliefs_provenance,
        });
    }

    // Layer 3: Query neighborhood (1-hop adjacency of top entities).
    // For M2, skip if no entities found.
    // (Would call query::load_adjacency + neighbors, but keeping M2 simple.)

    // When hints request a wider context, fetch more recent episodes.
    let recent_limit: i64 = if let Some(h) = hints {
        if h.is_session_start || h.topic_changed {
            20
        } else {
            5
        }
    } else {
        5
    };

    // Layer 4: Recent episodes.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
             WHERE space_id = ?1 AND redacted_at IS NULL AND content != ''
             ORDER BY ingested_at DESC LIMIT ?2",
        )
        .map_err(sql_err)?;
    let recent: Vec<(String, String)> = stmt
        .query_map(params![space, recent_limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    if !recent.is_empty() {
        let mut episode_text = String::new();
        let mut episode_prov: Vec<String> = Vec::new();
        for (id, content) in &recent {
            let remaining = budget.saturating_sub(total_tokens);
            if tokenizer.count(content) > remaining {
                truncated = true;
                break;
            }
            episode_text.push_str(content);
            episode_text.push('\n');
            episode_prov.push(id.clone());
            total_tokens += tokenizer.count(content);
        }
        if !episode_text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::RecentEpisodes,
                text: episode_text.clone(),
                estimated_tokens: tokenizer.count(&episode_text),
                provenance: episode_prov,
            });
        }
    }

    Ok(ContextResult {
        layers,
        total_tokens,
        budget: ContextBudget { max_tokens: budget },
        truncated,
    })
}
/// Rendered belief used by `assemble_context`. The legacy struct dropped
/// the subject (F6); the rewritten version includes subject, canonical key,
/// validity interval, status, confidence, support, and sources.
struct RenderedBelief {
    subject: String,
    canonical_key: String,
    predicate: String,
    object: String,
    valid_from: i64,
    valid_to: i64,
    status: String,
    confidence: f64,
    distinct_episodes: u32,
    text: String,
}

fn render_belief(
    conn: &Connection,
    space: &str,
    statement_id: &str,
) -> Result<RenderedBelief, BrainError> {
    let row = conn
        .query_row(
            "SELECT s.subject_id, ek.surface, s.predicate,
                COALESCE(s.object_entity, s.object_literal, ''),
                b.status, b.valid_from, b.valid_to,
                b.confidence, b.support_json
         FROM statements s
         LEFT JOIN entity_keys ek
                ON ek.entity_id = s.subject_id
                AND ek.type_name = (SELECT type_name FROM entities WHERE id = s.subject_id)
                AND ek.origin = 'canonical'
         LEFT JOIN beliefs b ON b.statement_id = s.id
         WHERE s.id = ?1 AND s.space_id = ?2
         ORDER BY b.valid_from DESC LIMIT 1",
            params![statement_id, space],

            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<f64>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .map_err(sql_err)?;
    let (subject, canonical_key, predicate, object_repr, status, valid_from, valid_to, confidence, support_json) = row;
    let canonical_key = canonical_key.unwrap_or_else(|| subject.clone());
    let status = status.unwrap_or_else(|| "active".into());
    let valid_from = valid_from.unwrap_or(0);
    let valid_to = valid_to.unwrap_or(oxibrain_ports::TIME_MAX.0);
    let confidence = confidence.unwrap_or(1.0);
    let distinct_episodes = support_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("distinct_episodes").and_then(|n| n.as_u64()))
        .unwrap_or(0) as u32;
    let validity = if valid_to == oxibrain_ports::TIME_MAX.0 {
        format!("from {valid_from} (open)")
    } else if valid_from == 0 {
        format!("until {valid_to}")
    } else {
        format!("{valid_from}..{valid_to}")
    };
    let text = format!(
        "{canonical_key} {predicate} {object_repr} ({status}, conf={confidence:.2}, {validity})"
    );
    Ok(RenderedBelief {
        subject,
        canonical_key,
        predicate,
        object: object_repr,
        valid_from,
        valid_to,
        status,
        confidence,
        distinct_episodes,
        text,
    })
}
