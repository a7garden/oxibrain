//! Context assembly: packing policy for agent memory (DESIGN §9.5).

use crate::query;
use crate::sql_err;
use oxibrain_core::context::{ContextBudget, ContextResult};
use oxibrain_core::retrieval::{Query, QueryMode};
use oxibrain_ports::{BrainError, TokenizerPort};
use rusqlite::{Connection, OptionalExtension, params};

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
///
/// This is the §12.3 reconstruction path: Profile (predicates marked
/// `profile_relevant`), high-salience beliefs with subjects (F6), the
/// query neighborhood, summaries with sources (§12.4), and recent
/// episodes — handed to `core::pack`, which decides what fits the budget.
pub fn assemble_context(
    conn: &Connection,
    space: &str,
    query_text: &str,
    budget: usize,
    hints: Option<&RecallHints>,
    tokenizer: &dyn TokenizerPort,
) -> Result<ContextResult, BrainError> {
    let policy = oxibrain_core::pack::PackPolicy::for_budget(budget);
    let mut input = oxibrain_core::pack::ContextInput::default();

    // 1. Profile — beliefs whose predicate is profile_relevant (§12.2).
    load_profile(conn, space, &mut input)?;

    // 2. High-salience beliefs for query-relevant entities (F6 subjects).
    let q = Query {
        text: query_text.to_string(),
        mode: QueryMode::Hybrid,
        space: space.to_string(),
        as_of: None,
        limit: 10,
        min_confidence: 0.0,
    };
    let ranking = query::hybrid_query(conn, &q, None)?;
    for item in &ranking.items {
        if let oxibrain_core::TargetId::Statement { id } = &item.target {
            if let Some(entry) = render_belief(conn, space, id)? {
                input.beliefs.push(entry);
            }
        }
    }

    // 3. Query neighborhood — 1-hop edges of the top entities.
    load_neighborhood(conn, space, &ranking, &mut input)?;

    // 4. Recent episodes (wider when hints request it).
    let recent_limit: i64 = if let Some(h) = hints {
        if h.is_session_start || h.topic_changed {
            20
        } else {
            5
        }
    } else {
        5
    };
    let mut stmt = conn
        .prepare(
            "SELECT id, content, ingested_at FROM episodes
             WHERE space_id = ?1 AND redacted_at IS NULL AND content != ''
             ORDER BY ingested_at DESC LIMIT ?2",
        )
        .map_err(sql_err)?;
    let recent = stmt
        .query_map(params![space, recent_limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    for (id, content, ingested_at) in recent {
        input.episodes.push(oxibrain_core::pack::EpisodeExcerpt {
            episode_id: id,
            content,
            ingested_at,
            salience: 0.5,
        });
    }

    // 5. Summaries with sources (§12.4) — cached Derived-episode summaries.
    load_summaries(conn, space, &mut input)?;

    Ok(oxibrain_core::pack::pack(
        &input,
        &ContextBudget { max_tokens: budget },
        &policy,
        tokenizer,
    ))
}

/// Profile: beliefs where subject is a pinned/high-salience entity AND the
/// predicate is marked `profile_relevant` in the registry (§12.2).
/// No pin API exists yet (M4), so the profile is the union of all
/// profile_relevant beliefs — a standing query, not a new store.
fn load_profile(
    conn: &Connection,
    space: &str,
    input: &mut oxibrain_core::pack::ContextInput,
) -> Result<(), BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT b.statement_id, s.subject_id, e.type_name, s.predicate,
                    s.object_entity, s.object_literal,
                    b.valid_from, b.valid_to, b.confidence
             FROM beliefs b
             JOIN statements s ON s.id = b.statement_id
             JOIN entities e ON e.id = s.subject_id
             WHERE s.space_id = ?1
               AND b.status = 'active'
               AND b.confidence >= 0.5
               AND s.predicate IN (
                   SELECT name FROM predicates
                   WHERE json_extract(def_json, '$.profile_relevant') = 1
               )
             ORDER BY b.confidence DESC
             LIMIT 30",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, f64>(8)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (stmt_id, subject, ty, predicate, obj_entity, obj_literal, vf, vt, conf) =
            row.map_err(sql_err)?;
        // Surface resolution: the subject's canonical key and the object's
        // surface (entity → canonical entity_key surface, literal → value).
        let subject_surface = canonical_surface(conn, &subject)?;
        let object_surface = match (&obj_entity, &obj_literal) {
            (Some(eid), _) => canonical_surface(conn, eid)?.unwrap_or_else(|| eid.clone()),
            (_, Some(lit)) => lit.clone(),
            _ => String::new(),
        };
        let subject_surface = subject_surface.unwrap_or_else(|| subject.clone());
        input.profile.push(oxibrain_core::pack::ProfileFact {
            subject: subject.clone(),
            canonical_key: format!("{subject_surface}:{ty}"),
            predicate,
            text: object_surface,
            valid_from: vf,
            valid_to: vt,
            confidence: conf as f32,
            trust: oxibrain_core::TrustTier::Trusted,
            sources: vec![stmt_id],
        });
    }
    Ok(())
}

/// Canonical surface form of an entity id — the surface of the key the
/// entity's `canonical_key` FK points at; fall back to any key on the
/// entity (user_declared preferred) when the FK is unset.
fn canonical_surface(conn: &Connection, entity_id: &str) -> Result<Option<String>, BrainError> {
    let via_fk = conn
        .query_row(
            "SELECT ek.surface FROM entities e
             JOIN entity_keys ek ON ek.id = e.canonical_key
             WHERE e.id = ?1",
            params![entity_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    if via_fk.is_some() {
        return Ok(via_fk);
    }
    conn.query_row(
        "SELECT surface FROM entity_keys
         WHERE entity_id = ?1
         ORDER BY CASE origin WHEN 'user_declared' THEN 0 WHEN 'extracted' THEN 1 ELSE 2 END
         LIMIT 1",
        params![entity_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(sql_err)
}

/// Neighborhood: 1-hop belief-filtered edges from the top ranked entities.
fn load_neighborhood(
    conn: &Connection,
    space: &str,
    ranking: &oxibrain_core::rank::RankingResult,
    input: &mut oxibrain_core::pack::ContextInput,
) -> Result<(), BrainError> {
    // Collect the top entity ids from the ranking.
    let mut entities: Vec<String> = Vec::new();
    for item in &ranking.items {
        if let oxibrain_core::TargetId::Entity { id } = &item.target {
            entities.push(id.clone());
        }
    }
    if entities.is_empty() {
        return Ok(());
    }
    entities.truncate(5);
    let placeholders = entities.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT s.subject_id, s.object_entity, s.predicate, s.id, b.confidence
         FROM statements s
         INNER JOIN beliefs b ON b.statement_id = s.id
         WHERE s.space_id = ?1
           AND s.object_entity IS NOT NULL
           AND b.status IN ('active', 'superseded')
           AND (s.subject_id IN ({placeholders}) OR s.object_entity IN ({placeholders}))
         LIMIT 30"
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&space as &dyn rusqlite::ToSql];
    for e in entities.iter().chain(entities.iter()) {
        params_vec.push(e as &dyn rusqlite::ToSql);
    }
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (from, to, predicate, stmt_id, conf) = row.map_err(sql_err)?;
        input.neighborhood.push(oxibrain_core::pack::RenderedEdge {
            from,
            to,
            predicate,
            statement_id: stmt_id,
            confidence: conf as f32,
        });
    }
    Ok(())
}

/// Summaries: cached Derived-episode summaries, paired with their sources
/// (§12.4 — a summary never travels without its sources).
fn load_summaries(
    conn: &Connection,
    space: &str,
    input: &mut oxibrain_core::pack::ContextInput,
) -> Result<(), BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
             WHERE space_id = ?1 AND kind = 'derived'
               AND redacted_at IS NULL AND content != ''
             ORDER BY ingested_at DESC LIMIT 5",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (id, content) = row.map_err(sql_err)?;
        input
            .summaries
            .push(oxibrain_core::pack::SummaryWithUncertainty {
                summary_id: id.clone(),
                text: content,
                confidence: 0.5,
                sources: vec![id],
            });
    }
    Ok(())
}
/// Render a statement as a `core::pack::RenderedBelief` — subject, canonical
/// key, validity, confidence, status, support (F6). Returns `None` when the
/// statement has no belief row (a legacy statement that never folded).
fn render_belief(
    conn: &Connection,
    space: &str,
    statement_id: &str,
) -> Result<Option<oxibrain_core::pack::RenderedBelief>, BrainError> {
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
    let (
        subject,
        canonical_key,
        predicate,
        object_repr,
        status,
        valid_from,
        valid_to,
        confidence,
        support_json,
    ) = row;
    let status = status.unwrap_or_else(|| "active".into());
    let belief_status = oxibrain_core::BeliefStatus::parse_db(&status)
        .unwrap_or(oxibrain_core::BeliefStatus::Active);
    let canonical_key = canonical_key.unwrap_or_else(|| subject.clone());
    let valid_from = valid_from.unwrap_or(0);
    let valid_to = valid_to.unwrap_or(oxibrain_ports::TIME_MAX.0);
    let confidence = confidence.unwrap_or(1.0) as f32;
    let distinct_episodes = support_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("distinct_episodes").and_then(|n| n.as_u64()))
        .unwrap_or(0) as u32;
    Ok(Some(oxibrain_core::pack::RenderedBelief {
        statement_id: statement_id.to_string(),
        subject,
        subject_canonical_key: canonical_key,
        predicate,
        object: object_repr,
        valid_from,
        valid_to,
        confidence,
        status: belief_status,
        support_episodes: distinct_episodes,
        sources: vec![],
    }))
}
