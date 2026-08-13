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
use oxibrain_index::ngram;
use oxibrain_index::{BlockingConfig, LshIndex};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// Per-(space, type) cache of the LSH blocking index and the entity-key set it
/// was built over. Lives as a `Mutex<ResolutionCache>` field on `Brain` so the
/// O(N) MinHash/LSH build happens once per (space, type) for the process
/// lifetime, not once per `declare` call. New keys are added incrementally via
/// [`insert_key`](Self::insert_key) (O(1)), so the cache stays in sync without
/// a full rebuild.
pub struct ResolutionCache {
    /// `(space_id, type_name)` → (LSH index over `keys`, the keys themselves).
    entries: HashMap<(String, String), (LshIndex, Vec<EntityKey>)>,
}

impl ResolutionCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Get or build the LSH index for `(space, type)`. Building reads all keys
    /// for the type from the projection and constructs the MinHash/LSH index
    /// over their normalized surfaces. Once built, the entry persists for the
    /// process lifetime and is updated incrementally by [`insert_key`].
    fn get_or_build(
        &mut self,
        conn: &Connection,
        space: &str,
        ty: &str,
    ) -> Result<(&LshIndex, &[EntityKey]), BrainError> {
        let key = (space.to_string(), ty.to_string());
        if !self.entries.contains_key(&key) {
            let config = BlockingConfig::default();
            let all_keys = kcrud::find_keys_for_type(conn, space, ty)?;
            let shingle_sets: Vec<BTreeSet<String>> = all_keys
                .iter()
                .map(|k| ngram::shingles(&k.normalized, 3))
                .collect();
            let index = LshIndex::build(&shingle_sets, &config);
            self.entries.insert(key.clone(), (index, all_keys));
        }
        let (idx, keys) = self.entries.get(&key).expect("just inserted");
        Ok((idx, keys.as_slice()))
    }

    /// Incrementally add a new entity key to the cached index for `(space, ty)`.
    /// O(1): computes the key's MinHash signature and inserts band entries
    /// pointing at the new position. If no cache entry exists for `(space, ty)`
    /// yet, this is a no-op — the next [`get_or_build`] reads all keys from the
    /// DB, including this new one.
    ///
    /// Call this after `insert_entity_key` returns `true` (row actually inserted).
    pub fn insert_key(&mut self, space: &str, ty: &str, key: &EntityKey) {
        let entry_key = (space.to_string(), ty.to_string());
        if let Some((index, all_keys)) = self.entries.get_mut(&entry_key) {
            let pos = all_keys.len();
            let shingles = ngram::shingles(&key.normalized, 3);
            index.insert(&shingles, pos);
            all_keys.push(key.clone());
        }
    }

    /// Drop the cached index for `(space, type)`, forcing a full rebuild on the
    /// next lookup. Used by `reproject` (which starts from an empty projection)
    /// and batch-import paths that change many keys at once.
    pub fn invalidate(&mut self, space: &str, ty: &str) {
        self.entries.remove(&(space.to_string(), ty.to_string()));
    }

    /// Clear all entries. Used by `reproject` before rebuilding the projection.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for ResolutionCache {
    fn default() -> Self {
        Self::new()
    }
}
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_or_create(
    conn: &Connection,
    space: &str,
    eref: &EntityRef,
    episode_id: &str,
    span_start: u32,
    now: Timestamp,
    context: &[String],
    cache: &mut ResolutionCache,
) -> Result<(String, ResolutionMethod), BrainError> {
    let normalized = resolution::normalize(&eref.surface, &eref.ty);
    // M9 §10.1 + caching: build the LSH blocking index once per (space, type)
    // per projection batch, not once per mention. The cache is populated
    // lazily on first lookup and reused for every subsequent mention of the
    // same type. See `ResolutionCache` for the per-(space, type) keys+index.
    let (index, all_keys) = cache.get_or_build(conn, space, &eref.ty)?;
    // M9 §10.1: block via MinHash/LSH over 3-gram shingles + entropy gate, plus
    // exact key hits. Sublinear candidate generation instead of a full scan.
    let candidates = block_candidates(all_keys, &normalized, index);

    // Precompute context neighbours once, outside the per-candidate closure.
    let context_neighbors: Vec<BTreeSet<String>> = context
        .iter()
        .map(|c| neighbor_set(conn, space, c))
        .collect();

    let decision = resolution::resolve(
        &normalized,
        &eref.ty,
        &candidates,
        |candidate| graph_context(conn, space, candidate, &context_neighbors),
        |candidate| embedding_sim(conn, candidate),
        &ResolutionConfig::default(),
    );

    match decision {
        oxibrain_core::resolution::Decision::Link { entity, method, .. } => {
            // Add this surface as a key if it doesn't already exist.
            let kid = entity_key_id(&entity, &normalized, &eref.ty);
            let new_key = EntityKey {
                id: kid,
                space: space.into(),
                entity: entity.clone(),
                ty: eref.ty.clone(),
                normalized: normalized.clone(),
                surface: eref.surface.clone(),
                origin: KeyOrigin::UserDeclared,
            };
            let inserted = kcrud::insert_entity_key(conn, &new_key)?;
            if inserted {
                cache.insert_key(space, &eref.ty, &new_key);
            }
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
            let new_key = EntityKey {
                id: kid,
                space: space.into(),
                entity: eid.clone(),
                ty: eref.ty.clone(),
                normalized,
                surface: eref.surface.clone(),
                origin: KeyOrigin::UserDeclared,
            };
            kcrud::insert_entity_key(conn, &new_key)?;
            // Incremental cache update: the new key is visible to subsequent
            // mentions of the same type without a full O(N) rebuild.
            cache.insert_key(space, &eref.ty, &new_key);
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
            let new_key = EntityKey {
                id: kid,
                space: space.into(),
                entity: eid.clone(),
                ty: eref.ty.clone(),
                normalized,
                surface: eref.surface.clone(),
                origin: KeyOrigin::UserDeclared,
            };
            kcrud::insert_entity_key(conn, &new_key)?;
            // Record merge candidate (not auto-merged).
            // For M1, we just create the entity; the merge candidate is visible
            // via entity_merges table queries. The new entity is returned.
            let _ = existing; // merge candidate recording is deferred to review tooling (M4)
            cache.insert_key(space, &eref.ty, &new_key);
            Ok((eid, ResolutionMethod::New))
        }
    }
}

/// M9 §10.1 candidate blocking: exact key hits plus MinHash/LSH candidates over
/// 3-gram shingles, gated on shingle entropy. Low-entropy surfaces (short or
/// repetitive) skip the fuzzy path — they only match exactly.
///
/// `index` is the LSH index over `all_keys` — the caller (resolution cache)
/// supplies it pre-built so multiple mentions against the same `(space, type)`
/// share one index build.
fn block_candidates(all_keys: &[EntityKey], normalized: &str, index: &LshIndex) -> Vec<EntityKey> {
    let config = BlockingConfig::default();
    let mention_shingles = ngram::shingles(normalized, 3);
    let mut idxs: BTreeSet<usize> = index
        .candidates(&mention_shingles, config.min_entropy)
        .into_iter()
        .collect();
    // Exact hits always pass the block, regardless of entropy.
    for (i, k) in all_keys.iter().enumerate() {
        if k.normalized == normalized {
            idxs.insert(i);
        }
    }
    idxs.into_iter().map(|i| all_keys[i].clone()).collect()
}

/// The set of entities sharing a statement with `entity_id` (as subject or
/// object) — the adjacency used for graph-context overlap (§10.2).
fn neighbor_set(conn: &Connection, space: &str, entity_id: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT subject_id, object_entity FROM statements
          WHERE space_id = ?1 AND (subject_id = ?2 OR object_entity = ?2)",
    ) else {
        return set;
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![space, entity_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    }) else {
        return set;
    };
    for row in rows.flatten() {
        let (subject, object) = row;
        if subject == entity_id {
            if let Some(obj) = object {
                set.insert(obj);
            }
        } else if object.as_deref() == Some(entity_id) {
            set.insert(subject);
        }
    }
    set
}

/// Graph-context overlap (§10.2): mean Jaccard similarity between the
/// candidate's neighbours and each context entity's neighbours. Non-zero when
/// a co-occurring entity shares neighbours with the candidate.
fn graph_context(
    conn: &Connection,
    space: &str,
    candidate: &str,
    context_neighbors: &[BTreeSet<String>],
) -> f64 {
    if context_neighbors.is_empty() {
        return 0.0;
    }
    let cn = neighbor_set(conn, space, candidate);
    let mut sum = 0.0;
    for ctx in context_neighbors {
        let intersection = cn.intersection(ctx).count();
        let union = cn.len() + ctx.len() - intersection;
        if union > 0 {
            sum += intersection as f64 / union as f64;
        }
    }
    sum / context_neighbors.len() as f64
}

/// Embedding similarity (§10.3) for a candidate entity. Resolution runs during
/// projection, before dense embeddings are computed (they are applied
/// post-projection via `embed_entities`), so this is zero here — but the caller
/// passes a real value rather than hardcoding it (that is how F13 happened).
///
/// **Architectural decision:** see `doc/adr/ADR-004-embedding-sim-zero-during-projection.md`.
/// The PerType weights in `ResolutionConfig` (Person/Org 0.1, Concept 0.6,
/// default 0.3) are set but inert until the decision is revisited. The
/// projection path stays deterministic (P1) and the embedder cannot poison
/// resolution through a stale or empty vector.
fn embedding_sim(_conn: &Connection, _candidate: &str) -> f64 {
    0.0
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
#[allow(clippy::too_many_arguments)]
fn resolve_object(
    conn: &Connection,
    space: &str,
    obj: &DeclObject,
    episode_id: &str,
    span_start: u32,
    now: Timestamp,
    context: &[String],
    cache: &mut ResolutionCache,
) -> Result<ResolvedObject, BrainError> {
    match obj {
        DeclObject::Entity { surface, ty } => {
            let eref = EntityRef {
                surface: surface.clone(),
                ty: ty.clone(),
            };
            let (eid, method) = resolve_or_create(
                conn, space, &eref, episode_id, span_start, now, context, cache,
            )?;
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
/// `cache` is the persistent `ResolutionCache` from `Brain` (or a fresh local
/// cache during `reproject`). The LSH blocking index is built once per
/// (space, type) and updated incrementally via `insert_key` when new keys are
/// added, so there is no per-call O(N) rebuild.
#[allow(clippy::too_many_arguments)]
pub fn project_declaration(
    conn: &Connection,
    space: &str,
    decl: &Declaration,
    now: Timestamp,
    cache: &mut ResolutionCache,
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
            let (subj_id, subj_method) =
                resolve_or_create(conn, space, subject, &ep_id, 0, now, &[], cache)?;

            // Resolve object with the subject as graph context (§10.2).
            let obj_resolved = resolve_object(
                conn,
                space,
                object,
                &ep_id,
                100,
                now,
                &[subj_id.clone()],
                cache,
            )?;

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
            let (loser_id, _) = resolve_or_create(conn, space, loser, &ep_id, 0, now, &[], cache)?;
            let (winner_id, _) = resolve_or_create(
                conn,
                space,
                winner,
                &ep_id,
                200,
                now,
                &[loser_id.clone()],
                cache,
            )?;

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
            episode: _target_ep,
        } => {
            // Resolve the statement to retract.
            let (subj_id, _) = resolve_or_create(conn, space, subject, &ep_id, 0, now, &[], cache)?;
            let obj_resolved = resolve_object(
                conn,
                space,
                object,
                &ep_id,
                100,
                now,
                &[subj_id.clone()],
                cache,
            )?;
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
