//! Declaration → projection pipeline (DESIGN §5.3, §8). A declaration creates a
//! Declaration episode, then projects it: resolve entities, create statements/
//! assertions/mentions, re-fold the affected group, update beliefs.
//! All in one transaction on the writer-actor connection.

use crate::knowledge as kcrud;
use crate::ledger;
use crate::registry;
use crate::sql_err;
use oxibrain_core::canonical::canonical_json_value;
use oxibrain_core::confidence::CalibrationTable;
use oxibrain_core::fold::fold;
use oxibrain_core::id::{assertion_id, entity_id, entity_key_id, mention_id, statement_id};
use oxibrain_core::knowledge::{
    Assertion, Entity, EntityKey, KeyOrigin, Mention, MentionRole, Object, Polarity,
    ResolutionMethod, Statement, TypedValue,
};
use oxibrain_core::resolution::{self, ResolutionConfig};
use oxibrain_core::{EpisodeKind, SourceRef};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// A reference to an entity by surface form + type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRef {
    pub surface: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// The object of a declaration statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeclObject {
    Entity {
        surface: String,
        #[serde(rename = "type")]
        ty: String,
    },
    Literal {
        literal_type: String,
        value: String,
    },
}

/// A declaration operation, serialized as the content of a Declaration episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Declaration {
    AddStatement {
        subject: EntityRef,
        predicate: String,
        object: DeclObject,
        #[serde(default = "default_polarity")]
        polarity: String,
        valid_from: i64,
        valid_to: i64,
    },
    Merge {
        loser: EntityRef,
        winner: EntityRef,
    },
    Retract {
        subject: EntityRef,
        predicate: String,
        object: DeclObject,
        episode: String,
    },
}

fn default_polarity() -> String {
    "affirm".to_string()
}

/// Canonical JSON for a declaration (sorted keys, compact).
pub fn canonical_declaration_content(decl: &Declaration) -> String {
    let v = serde_json::to_value(decl).expect("declaration serializable");
    canonical_json_value(&v)
}

/// Parse a declaration from JSON content.
pub fn parse_declaration(content: &str) -> Result<Declaration, BrainError> {
    serde_json::from_str(content)
        .map_err(|e| BrainError::Invalid(format!("declaration parse: {e}")))
}

/// Resolve or create an entity from a surface form + type.
/// Returns (entity_id, mention_method).
pub(crate) fn resolve_or_create(
    conn: &Connection,
    space: &str,
    eref: &EntityRef,
    episode_id: &str,
    span_start: u32,
    now: Timestamp,
) -> Result<(String, ResolutionMethod), BrainError> {
    let normalized = resolution::normalize(&eref.surface, &eref.ty);
    let candidates = kcrud::find_keys_for_type(conn, space, &eref.ty)?;

    let decision = resolution::resolve(
        &normalized,
        &eref.ty,
        &candidates,
        |_| 0.0, // graph context: wired, zero until M9 (F13)
        |_| 0.0, // embedding sim: wired, zero until M7.3/M9
        &ResolutionConfig::default(),
    );

    match decision {
        oxibrain_core::resolution::Decision::Link { entity, method, .. } => {
            // Add this surface as a key if it doesn't exist.
            let kid = entity_key_id(&entity, &normalized, &eref.ty);
            kcrud::insert_entity_key(
                conn,
                &EntityKey {
                    id: kid,
                    space: space.into(),
                    entity: entity.clone(),
                    ty: eref.ty.clone(),
                    normalized: normalized.clone(),
                    surface: eref.surface.clone(),
                    origin: KeyOrigin::UserDeclared,
                },
            )?;
            Ok((entity, method))
        }
        oxibrain_core::resolution::Decision::New { method, .. } => {
            // Create a new entity.
            let eid = entity_id(space, &eref.ty, episode_id, span_start);
            kcrud::insert_entity(
                conn,
                &Entity {
                    id: eid.clone(),
                    space: space.into(),
                    ty: eref.ty.clone(),
                    canonical_key: None,
                    created_at: now,
                    merged_into: None,
                },
            )?;
            let kid = entity_key_id(&eid, &normalized, &eref.ty);
            kcrud::insert_entity_key(
                conn,
                &EntityKey {
                    id: kid,
                    space: space.into(),
                    entity: eid.clone(),
                    ty: eref.ty.clone(),
                    normalized,
                    surface: eref.surface.clone(),
                    origin: KeyOrigin::UserDeclared,
                },
            )?;
            Ok((eid, method))
        }
        oxibrain_core::resolution::Decision::Candidate { existing, .. } => {
            // Create a new entity AND record a merge candidate.
            let eid = entity_id(space, &eref.ty, episode_id, span_start);
            kcrud::insert_entity(
                conn,
                &Entity {
                    id: eid.clone(),
                    space: space.into(),
                    ty: eref.ty.clone(),
                    canonical_key: None,
                    created_at: now,
                    merged_into: None,
                },
            )?;
            let normalized = resolution::normalize(&eref.surface, &eref.ty);
            let kid = entity_key_id(&eid, &normalized, &eref.ty);
            kcrud::insert_entity_key(
                conn,
                &EntityKey {
                    id: kid,
                    space: space.into(),
                    entity: eid.clone(),
                    ty: eref.ty.clone(),
                    normalized,
                    surface: eref.surface.clone(),
                    origin: KeyOrigin::UserDeclared,
                },
            )?;
            // Record merge candidate (not auto-merged).
            // For M1, we just create the entity; the merge candidate is visible
            // via entity_merges table queries. The new entity is returned.
            let _ = existing; // merge candidate recording is deferred to review tooling (M4)
            Ok((eid, ResolutionMethod::New))
        }
    }
}

/// Convert a DeclObject to an Object, resolving entity refs.
/// Result of resolving a declaration object: the Object plus the surface/method
/// needed to capture the object mention (entity objects only).
struct ResolvedObject {
    object: Object,
    /// (entity_id, resolution_method) for entity objects; None for literals.
    entity: Option<(String, ResolutionMethod)>,
    surface: String,
    ty: String,
}

fn resolve_object(
    conn: &Connection,
    space: &str,
    obj: &DeclObject,
    episode_id: &str,
    span_start: u32,
    now: Timestamp,
) -> Result<ResolvedObject, BrainError> {
    match obj {
        DeclObject::Entity { surface, ty } => {
            let eref = EntityRef {
                surface: surface.clone(),
                ty: ty.clone(),
            };
            let (eid, method) = resolve_or_create(conn, space, &eref, episode_id, span_start, now)?;
            Ok(ResolvedObject {
                object: Object::Entity(eid.clone()),
                entity: Some((eid, method)),
                surface: surface.clone(),
                ty: ty.clone(),
            })
        }
        DeclObject::Literal {
            literal_type,
            value,
        } => {
            let tv = parse_literal(literal_type, value)?;
            Ok(ResolvedObject {
                object: Object::Literal(tv),
                entity: None,
                surface: value.clone(),
                ty: literal_type.clone(),
            })
        }
    }
}

fn parse_literal(lt: &str, value: &str) -> Result<TypedValue, BrainError> {
    match lt {
        "text" => Ok(TypedValue::Text(value.into())),
        "date" => Ok(TypedValue::Date(value.into())),
        "datetime" => Ok(TypedValue::DateTime(value.into())),
        "number" => {
            let n: f64 = value
                .parse()
                .map_err(|e| BrainError::Invalid(format!("number: {e}")))?;
            Ok(TypedValue::Number(n))
        }
        "bool" => {
            let b: bool = value
                .parse()
                .map_err(|e| BrainError::Invalid(format!("bool: {e}")))?;
            Ok(TypedValue::Bool(b))
        }
        _ => Err(BrainError::Invalid(format!("unknown literal type: {lt}"))),
    }
}

/// Project a declaration: write episode, resolve entities, create assertions,
/// re-fold affected group, update beliefs. All in one transaction.
pub fn project_declaration(
    conn: &Connection,
    space: &str,
    decl: &Declaration,
    now: Timestamp,
) -> Result<String, BrainError> {
    // `now` is the transaction time: `recorded_at`, `occurred_at`, `ingested_at`.
    // Callers pass the current wall clock (facade) or an episode's stored
    // ingested_at (reproject) so the derived ids/timestamps are deterministic.

    // 1. Build canonical content + episode.
    let content = canonical_declaration_content(decl);
    let ch = oxibrain_core::content_hash(&content);
    let source = SourceRef::Declaration;
    let occurred_at = now;
    let ep_id = oxibrain_core::episode_id(space, &ch, &source, occurred_at);

    // Insert the Declaration episode (idempotent).
    let mut episode = oxibrain_core::Episode {
        id: ep_id.clone(),
        space: space.into(),
        seq: 0, // assigned by insert_episode
        content_hash: ch,
        content: content.clone(),
        source,
        trust: oxibrain_core::TrustTier::Trusted,
        kind: EpisodeKind::Declaration,
        occurred_at,
        ingested_at: now,
        redacted_at: None,
    };
    ledger::insert_episode(conn, &mut episode)?;
    let ep_id = episode.id.clone();

    // 2. Process the declaration.
    match decl {
        Declaration::AddStatement {
            subject,
            predicate,
            object,
            polarity,
            valid_from,
            valid_to,
        } => {
            let pol = match polarity.as_str() {
                "affirm" => Polarity::Affirm,
                "deny" => Polarity::Deny,
                other => return Err(BrainError::Invalid(format!("polarity: {other}"))),
            };

            // Resolve subject entity.
            let (subj_id, subj_method) = resolve_or_create(conn, space, subject, &ep_id, 0, now)?;

            // Resolve object.
            let obj_resolved = resolve_object(conn, space, object, &ep_id, 100, now)?;

            // Create statement (idempotent).
            let subj_for_hash = &subj_id;
            let stmt_id = statement_id(space, subj_for_hash, predicate, &obj_resolved.object);
            let stmt = Statement {
                id: stmt_id.clone(),
                space: space.into(),
                subject: subj_id.clone(),
                predicate: predicate.clone(),
                object: obj_resolved.object.clone(),
            };
            kcrud::insert_statement(conn, &stmt)?;

            // Create assertion (idempotent).
            let extractor_id = "declaration"; // None equivalent for declarations
            let aid = assertion_id(
                &stmt_id,
                &ep_id,
                extractor_id,
                pol,
                Timestamp(*valid_from),
                Timestamp(*valid_to),
                1.0,
            );
            let assertion = Assertion {
                id: aid.clone(),
                statement: stmt_id.clone(),
                episode: ep_id.clone(),
                extractor: None,
                polarity: pol,
                claimed_from: Timestamp(*valid_from),
                claimed_to: Timestamp(*valid_to),
                confidence: 1.0,
                recorded_at: now,
                retracted_at: None,
            };
            kcrud::insert_assertion(conn, &assertion)?;

            // Capture mentions.
            let subj_mention = Mention {
                id: mention_id(&aid, "subject", 0),
                assertion: aid.clone(),
                role: MentionRole::Subject,
                surface: subject.surface.clone(),
                span: (0, subject.surface.len() as u32),
                resolved_to: Some(subj_id.clone()),
                method: subj_method,
            };
            kcrud::insert_mention(conn, &subj_mention)?;

            if let Some((obj_entity_id, obj_method)) = obj_resolved.entity {
                let obj_mention = Mention {
                    id: mention_id(&aid, "object", 100),
                    assertion: aid.clone(),
                    role: MentionRole::Object,
                    surface: obj_resolved.surface,
                    span: (100, 100 + obj_resolved.ty.len() as u32),
                    resolved_to: Some(obj_entity_id),
                    method: obj_method,
                };
                kcrud::insert_mention(conn, &obj_mention)?;
            }

            // 3. Re-fold the affected group.
            let calibration = CalibrationTable::default();
            let pred_def = registry::load_predicate(conn, predicate)?
                .ok_or_else(|| BrainError::Invalid(format!("unknown predicate: {predicate}")))?;

            let group = kcrud::get_statement_group(conn, space, &subj_id, predicate)?;
            let beliefs = fold(&pred_def, &group, now, &calibration);

            // Collect all statement IDs in the group for belief replacement.
            let group_stmt_ids: Vec<String> =
                group.iter().map(|e| e.statement.id.clone()).collect();
            kcrud::replace_beliefs(conn, &group_stmt_ids, &beliefs)?;
        }
        Declaration::Merge { loser, winner } => {
            let (loser_id, _) = resolve_or_create(conn, space, loser, &ep_id, 0, now)?;
            let (winner_id, _) = resolve_or_create(conn, space, winner, &ep_id, 200, now)?;

            let merge_id = oxibrain_core::id::entity_merge_id(&loser_id, &winner_id, &ep_id);
            kcrud::insert_merge(
                conn,
                &oxibrain_core::EntityMerge {
                    id: merge_id,
                    loser: loser_id.clone(),
                    winner: winner_id.clone(),
                    decided_by: oxibrain_core::MergeDecision::User,
                    provenance: ep_id.clone(),
                    evidence: vec![],
                    decided_at: now,
                    undone_at: None,
                },
            )?;
            kcrud::set_merged_into(conn, &loser_id, &winner_id)?;
        }
        Declaration::Retract {
            subject,
            predicate,
            object,
            episode: target_ep,
        } => {
            // Resolve the statement to retract.
            let (subj_id, _) = resolve_or_create(conn, space, subject, &ep_id, 0, now)?;
            let obj_resolved = resolve_object(conn, space, object, &ep_id, 100, now)?;
            let stmt_id = statement_id(space, &subj_id, predicate, &obj_resolved.object);

            // Set retracted_at on ALL matching assertions (the retract is a
            // universal "this statement is no longer believed", not a per-
            // episode retraction). The episode_id filter in the original
            // M4 path caused retractions to silently no-op when the
            // retraction episode differed from the original assertion
            // episode (which is the common case).
            conn.execute(
                "UPDATE assertions SET retracted_at = ?1
                 WHERE statement_id = ?2 AND retracted_at IS NULL",
                rusqlite::params![now.millis(), stmt_id],
            )
            .map_err(sql_err)?;

            // Re-fold the affected group.
            let calibration = CalibrationTable::default();
            if let Some(pred_def) = registry::load_predicate(conn, predicate)? {
                let group = kcrud::get_statement_group(conn, space, &subj_id, predicate)?;
                let beliefs = fold(&pred_def, &group, now, &calibration);
                let group_stmt_ids: Vec<String> =
                    group.iter().map(|e| e.statement.id.clone()).collect();
                kcrud::replace_beliefs(conn, &group_stmt_ids, &beliefs)?;
            }
        }
    }

    Ok(ep_id)
}
