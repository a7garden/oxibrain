//! Context assembly: packing policy for agent memory (DESIGN §9.5).

use crate::query;
use crate::sql_err;
use oxibrain_core::context::{
    ContextBudget, ContextLayer, ContextResult, LayerKind, estimate_tokens,
};
use oxibrain_core::retrieval::{Query, QueryMode, SearchTarget};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Pack context for a query to a token budget.
pub fn assemble_context(
    conn: &Connection,
    space: &str,
    query_text: &str,
    budget: usize,
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
    let ranking = query::hybrid_query(conn, &q)?;
    let mut beliefs_text = String::new();
    let mut beliefs_provenance: Vec<String> = Vec::new();
    for item in &ranking.items {
        if let SearchTarget::Statement { id } = &item.target {
            let entry = render_belief(conn, space, id)?;
            beliefs_text.push_str(&entry.text);
            beliefs_text.push('\n');
            beliefs_provenance.push(id.clone());
        }
    }
    if !beliefs_text.is_empty() {
        let tokens = estimate_tokens(&beliefs_text);
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

    // Layer 4: Recent episodes.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
             WHERE space_id = ?1 AND redacted_at IS NULL AND content != ''
             ORDER BY ingested_at DESC LIMIT 5",
        )
        .map_err(sql_err)?;
    let recent: Vec<(String, String)> = stmt
        .query_map(params![space], |r| {
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
            if estimate_tokens(content) > remaining {
                truncated = true;
                break;
            }
            episode_text.push_str(content);
            episode_text.push('\n');
            episode_prov.push(id.clone());
            total_tokens += estimate_tokens(content);
        }
        if !episode_text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::RecentEpisodes,
                text: episode_text.clone(),
                estimated_tokens: estimate_tokens(&episode_text),
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

struct RenderedBelief {
    text: String,
}

fn render_belief(
    conn: &Connection,
    space: &str,
    statement_id: &str,
) -> Result<RenderedBelief, BrainError> {
    let row: (String, Option<String>, Option<String>, String, i64, f64) = conn
        .query_row(
            "SELECT s.predicate, s.object_entity, s.object_literal,
                    b.status, b.valid_from, b.confidence
             FROM statements s
             LEFT JOIN beliefs b ON b.statement_id = s.id
             WHERE s.id = ?1 AND s.space_id = ?2
             ORDER BY b.valid_from DESC LIMIT 1",
            params![statement_id, space],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, f64>(5)?,
                ))
            },
        )
        .map_err(sql_err)?;
    let (predicate, obj_entity, obj_literal, status, _valid_from, confidence) = row;
    let object_repr = obj_entity.or(obj_literal).unwrap_or_default();
    let text =
        format!("... {predicate} {object_repr} (status={status}, confidence={confidence:.2})");
    Ok(RenderedBelief { text })
}
