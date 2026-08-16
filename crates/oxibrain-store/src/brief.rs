//! Brief data fetch (M9 §14.1): assemble everything `oxibrain-views` needs to
//! render an entity page. Pure fetch — no decisions, no rendering (§18 rule 3:
//! store fetches, facade sequences, views render).

use crate::knowledge as kcrud;
use crate::query;
use crate::sql_err;
use crate::timeline;
use oxibrain_core::{Entity, EntityKey, Object, Statement, TypedValue};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// Everything needed to render `brief(entity)`.
pub struct EntityBriefData {
    pub entity: Entity,
    pub canonical_surface: String,
    pub aliases: Vec<String>,
    pub beliefs: Vec<BeliefRow>,
    pub neighbours: Vec<NeighbourData>,
    pub timeline: Vec<timeline::TimelineEntry>,
    pub sources: Vec<SourceData>,
    pub contradictions: Vec<ContradictionData>,
}

/// A current belief joined with its statement, with the object pre-rendered to
/// a display string so the facade does no further SQL.
pub struct BeliefRow {
    pub statement_id: String,
    pub predicate: String,
    pub object: String,
    pub object_entity: Option<String>,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub confidence: f32,
    pub affirm: u32,
    pub deny: u32,
    pub episodes: u32,
    pub status: String,
}

pub struct NeighbourData {
    pub surface: String,
    pub entity: String,
    pub predicate: String,
    /// `out` = this entity is the subject; `in` = this entity is the object.
    pub direction: String,
}

pub struct SourceData {
    pub episode: String,
    pub kind: String,
    pub occurred_at: Timestamp,
}

pub struct ContradictionData {
    pub predicate: String,
    pub object: String,
    pub affirm_episodes: Vec<String>,
    pub deny_episodes: Vec<String>,
}

/// Fetch the data for an entity brief. Follows the merge chain so a merged-away
/// id renders as its ultimate winner.
pub fn entity_brief(
    conn: &Connection,
    space: &str,
    entity_id: &str,
) -> Result<EntityBriefData, BrainError> {
    let resolved = kcrud::resolve_entity(conn, entity_id)?;
    let entity = kcrud::get_entity(conn, &resolved)?.ok_or_else(|| {
        BrainError::NotFound(format!("entity {resolved} not found in space {space}"))
    })?;

    let keys = load_keys(conn, &resolved)?;
    let canonical_surface = canonical_key_surface(&entity, &keys)
        .or_else(|| keys.first().map(|k| k.surface.clone()))
        .unwrap_or_default();
    let mut aliases: Vec<String> = keys
        .iter()
        .map(|k| k.surface.clone())
        .filter(|s| *s != canonical_surface)
        .collect();
    aliases.sort();
    aliases.dedup();

    let statements = load_statements(conn, space, &resolved)?;
    let beliefs = load_belief_rows(conn, space, &resolved, &statements)?;
    let neighbours = build_neighbours(conn, &statements, &resolved)?;
    // `timeline()` resolves `object_repr` to surfaces / plain literal values
    // in-store (display-only, falls back to raw on failure).
    let timeline = timeline::timeline(conn, space, &resolved, None, None)?;
    let sources = load_sources(conn, space, &resolved)?;
    let contradictions = load_contradictions(conn, &beliefs, &statements)?;

    Ok(EntityBriefData {
        entity,
        canonical_surface,
        aliases,
        beliefs,
        neighbours,
        timeline,
        sources,
        contradictions,
    })
}

fn load_keys(conn: &Connection, entity_id: &str) -> Result<Vec<EntityKey>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, entity_id, type_name, normalized, surface, origin
               FROM entity_keys WHERE entity_id = ?1 ORDER BY surface",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![entity_id], |r| {
            Ok(EntityKey {
                id: r.get(0)?,
                space: r.get(1)?,
                entity: r.get(2)?,
                ty: r.get(3)?,
                normalized: r.get(4)?,
                surface: r.get(5)?,
                origin: oxibrain_core::KeyOrigin::parse_db(&r.get::<_, String>(6)?)
                    .unwrap_or(oxibrain_core::KeyOrigin::Extracted),
            })
        })
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

fn canonical_key_surface(entity: &Entity, keys: &[EntityKey]) -> Option<String> {
    entity
        .canonical_key
        .as_ref()
        .and_then(|cid| keys.iter().find(|k| &k.id == cid))
        .map(|k| k.surface.clone())
}

fn load_statements(
    conn: &Connection,
    space: &str,
    entity_id: &str,
) -> Result<HashMap<String, Statement>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, subject_id, predicate, object_entity, object_literal
               FROM statements
              WHERE space_id = ?1 AND (subject_id = ?2 OR object_entity = ?2)
              ORDER BY id",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, entity_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(sql_err)?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, subject, predicate, object_entity, object_literal) = row.map_err(sql_err)?;
        let object = match (object_entity, object_literal) {
            (Some(eid), None) => Object::Entity(eid),
            (None, Some(lit)) => {
                Object::Literal(serde_json::from_str(&lit).expect("valid literal in db"))
            }
            _ => Object::Literal(TypedValue::Text(String::new())),
        };
        map.insert(
            id.clone(),
            Statement {
                id,
                space: space.to_string(),
                subject,
                predicate,
                object,
            },
        );
    }
    Ok(map)
}

fn load_belief_rows(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    statements: &HashMap<String, Statement>,
) -> Result<Vec<BeliefRow>, BrainError> {
    let beliefs = query::beliefs_for_entity(conn, space, entity_id)?;
    let mut rows = Vec::with_capacity(beliefs.len());
    for b in beliefs {
        let Some(s) = statements.get(&b.statement) else {
            continue;
        };
        let (object, object_entity) = match &s.object {
            Object::Entity(eid) => (surface_of(conn, eid)?, Some(eid.clone())),
            Object::Literal(tv) => (literal_repr(tv), None),
        };
        rows.push(BeliefRow {
            statement_id: b.statement.clone(),
            predicate: s.predicate.clone(),
            object,
            object_entity,
            valid_from: b.valid_from,
            valid_to: b.valid_to,
            confidence: b.confidence,
            affirm: b.support.affirm_count,
            deny: b.support.deny_count,
            episodes: b.support.distinct_episodes,
            status: b.status.as_db().to_string(),
        });
    }
    rows.sort_by(|a, b| (&a.predicate, &a.object).cmp(&(&b.predicate, &b.object)));
    Ok(rows)
}

fn build_neighbours(
    conn: &Connection,
    statements: &HashMap<String, Statement>,
    entity_id: &str,
) -> Result<Vec<NeighbourData>, BrainError> {
    let mut out: Vec<NeighbourData> = Vec::new();
    for s in statements.values() {
        if s.subject == entity_id {
            if let Object::Entity(eid) = &s.object {
                out.push(NeighbourData {
                    surface: surface_of(conn, eid)?,
                    entity: eid.clone(),
                    predicate: s.predicate.clone(),
                    direction: "out".to_string(),
                });
            }
        } else if let Object::Entity(eid) = &s.object {
            if eid == entity_id {
                out.push(NeighbourData {
                    surface: surface_of(conn, &s.subject)?,
                    entity: s.subject.clone(),
                    predicate: s.predicate.clone(),
                    direction: "in".to_string(),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (&a.surface, &a.predicate, &a.direction).cmp(&(&b.surface, &b.predicate, &b.direction))
    });
    out.dedup_by(|a, b| {
        a.entity == b.entity && a.predicate == b.predicate && a.direction == b.direction
    });
    Ok(out)
}

/// Canonical surface for an entity id (the display name used in links).
pub fn surface_of(conn: &Connection, entity_id: &str) -> Result<String, BrainError> {
    crate::index_ops::entity_surface(conn, entity_id)
}

fn load_sources(
    conn: &Connection,
    space: &str,
    entity_id: &str,
) -> Result<Vec<SourceData>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT e.id, e.source_kind, e.occurred_at
               FROM episodes e
               JOIN assertions a ON a.episode_id = e.id
               JOIN statements s ON a.statement_id = s.id
              WHERE s.space_id = ?1 AND (s.subject_id = ?2 OR s.object_entity = ?2)
              ORDER BY e.seq ASC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, entity_id], |r| {
            Ok(SourceData {
                episode: r.get(0)?,
                kind: r.get(1)?,
                occurred_at: Timestamp(r.get(2)?),
            })
        })
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

fn load_contradictions(
    conn: &Connection,
    beliefs: &[BeliefRow],
    statements: &HashMap<String, Statement>,
) -> Result<Vec<ContradictionData>, BrainError> {
    let mut out = Vec::new();
    for b in beliefs {
        if b.status != "contradicted" {
            continue;
        }
        let Some(s) = statements.get(&b.statement_id).cloned() else {
            continue;
        };
        let assertions = kcrud::get_assertions_for_statement(conn, &b.statement_id)?;
        let mut affirm: Vec<String> = Vec::new();
        let mut deny: Vec<String> = Vec::new();
        for a in &assertions {
            match a.polarity {
                oxibrain_core::Polarity::Affirm => affirm.push(a.episode.clone()),
                oxibrain_core::Polarity::Deny => deny.push(a.episode.clone()),
            }
        }
        affirm.sort();
        affirm.dedup();
        deny.sort();
        deny.dedup();
        out.push(ContradictionData {
            predicate: s.predicate.clone(),
            object: match &s.object {
                Object::Entity(eid) => surface_of(conn, eid)?,
                Object::Literal(tv) => literal_repr(tv),
            },
            affirm_episodes: affirm,
            deny_episodes: deny,
        });
    }
    out.sort_by(|a, b| (&a.predicate, &a.object).cmp(&(&b.predicate, &b.object)));
    Ok(out)
}

pub(crate) fn literal_repr(tv: &TypedValue) -> String {
    match tv {
        TypedValue::Text(s) | TypedValue::Date(s) | TypedValue::DateTime(s) => s.clone(),
        TypedValue::Enum(s) => s.clone(),
        TypedValue::Quantity { value, unit } => format!("{value} {unit}"),
        TypedValue::Number(n) => n.to_string(),
        TypedValue::Bool(b) => b.to_string(),
    }
}

// ── Space + Topic brief data ────────────────────────────────────────────────

/// Everything needed to render `brief(space)`. Pure fetch — no decisions.
pub struct SpaceBriefData {
    pub space_name: String,
    pub stats: SpaceCounts,
    pub top_entities: Vec<EntityLinkRow>,
}

/// Counts surfaced on a space brief (mirrors `oxibrain_core::SpaceStats`).
pub struct SpaceCounts {
    pub episodes: i64,
    pub entities: i64,
    pub statements: i64,
    pub contradictions: usize,
}

/// One entity rendered as a followable link in the space/topic brief list.
pub struct EntityLinkRow {
    pub surface: String,
    pub entity_id: String,
    pub predicate_count: usize,
}

/// Everything needed to render `brief(topic)`. Pure fetch.
pub struct TopicBriefData {
    pub topic: String,
    pub matched_entities: Vec<EntityLinkRow>,
}

/// Fetch the data for a space brief. `limit` caps the top-entities list.
pub fn space_brief(
    conn: &Connection,
    space: &str,
    limit: usize,
) -> Result<SpaceBriefData, BrainError> {
    // 1. Stats.
    let stats_row = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL),
                (SELECT COUNT(*) FROM entities WHERE space_id = ?1 AND merged_into IS NULL),
                (SELECT COUNT(*) FROM statements WHERE space_id = ?1),
                (SELECT COUNT(DISTINCT s.id) FROM statements s
                  WHERE s.space_id = ?1
                    AND EXISTS (SELECT 1 FROM statements s2
                                 WHERE s2.space_id = s.space_id
                                   AND s2.subject_id = s.subject_id
                                   AND s2.predicate = s.predicate
                                   AND s2.id != s.id))",
            params![space],
            |r| {
                Ok(SpaceCounts {
                    episodes: r.get(0)?,
                    entities: r.get(1)?,
                    statements: r.get(2)?,
                    contradictions: r.get::<_, i64>(3)? as usize,
                })
            },
        )
        .map_err(sql_err)?;

    // 2. Top entities by predicate_count (cheap salience proxy). Surfaces
    // are read from any key on the entity (canonical_key is not always set
    // by the projection path — `entity_brief` handles this by falling
    // back to the first key; we do the same here for consistency).
    let mut stmt = conn
        .prepare(
            "SELECT e.id,
                    COALESCE(
                        (SELECT surface FROM entity_keys
                          WHERE entity_id = e.id ORDER BY surface LIMIT 1),
                        e.id) AS surface,
                    (SELECT COUNT(DISTINCT predicate) FROM statements
                      WHERE space_id = ?1 AND (subject_id = e.id OR object_entity = e.id))
               AS pred_count
             FROM entities e
            WHERE e.space_id = ?2 AND e.merged_into IS NULL
            ORDER BY pred_count DESC, surface ASC
            LIMIT ?3",
        )
        .map_err(sql_err)?;
    let top_entities: Vec<EntityLinkRow> = stmt
        .query_map(params![space, space, limit as i64], |r| {
            Ok(EntityLinkRow {
                entity_id: r.get(0)?,
                surface: r.get(1)?,
                predicate_count: r.get::<_, i64>(2)? as usize,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;

    Ok(SpaceBriefData {
        space_name: space.to_string(),
        stats: stats_row,
        top_entities,
    })
}

/// Fetch the data for a topic brief. Matches entity keys whose surface
/// contains the topic (case-insensitive substring) — a lexical proxy that
/// does not require an embedder. The corpus is gated by `space`.
///
/// `limit` caps the match list. Duplicate entities (multiple keys with the
/// same surface match) are deduplicated by entity_id.
pub fn topic_brief(
    conn: &Connection,
    space: &str,
    topic: &str,
    limit: usize,
) -> Result<TopicBriefData, BrainError> {
    let pattern = format!("%{}%", topic.to_lowercase());
    let mut stmt = conn
        .prepare(
            "SELECT e.id, ek.surface,
                    (SELECT COUNT(DISTINCT predicate) FROM statements
                      WHERE space_id = ?1 AND (subject_id = e.id OR object_entity = e.id))
               AS pred_count
             FROM entity_keys ek
             JOIN entities e ON e.id = ek.entity_id
            WHERE ek.space_id = ?1 AND e.merged_into IS NULL
              AND LOWER(ek.surface) LIKE ?2
            ORDER BY pred_count DESC, ek.surface ASC
            LIMIT ?3",
        )
        .map_err(sql_err)?;
    let mut matched: Vec<EntityLinkRow> = stmt
        .query_map(params![space, pattern, (limit * 2) as i64], |r| {
            Ok(EntityLinkRow {
                entity_id: r.get(0)?,
                surface: r.get(1)?,
                predicate_count: r.get::<_, i64>(2)? as usize,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    // Deduplicate by entity_id (same entity may match via multiple keys),
    // keep the highest predicate_count row.
    matched.sort_by(|a, b| {
        b.predicate_count
            .cmp(&a.predicate_count)
            .then_with(|| a.surface.cmp(&b.surface))
    });
    let mut seen = std::collections::HashSet::new();
    matched.retain(|e| seen.insert(e.entity_id.clone()));
    matched.truncate(limit);

    Ok(TopicBriefData {
        topic: topic.to_string(),
        matched_entities: matched,
    })
}
