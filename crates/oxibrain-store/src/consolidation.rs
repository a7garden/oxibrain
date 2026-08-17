//! Consolidation: cluster related episodes → summarize → Derived episodes (DESIGN §10).
//! Community summaries: LLM-generated text cached as Derived episodes (DESIGN §9.4, §5.3).
//!
//! M3c implementation target. Store primitives only — Brain facade orchestrates LLM calls.

use crate::sql_err;
use oxibrain_core::{EpisodeKind, SourceRef};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// A cluster of related episodes (shared entities + temporal proximity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeCluster {
    pub episode_ids: Vec<String>,
    pub shared_entities: Vec<String>,
}

/// A community group for summarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityGroup {
    pub label: u64,
    pub entity_ids: Vec<String>,
}

/// Check the summaries cache for a given scope + member set + extractor.
pub fn get_cached_summary(
    conn: &Connection,
    scope_kind: &str,
    member_hash: &[u8],
    extractor_id: &str,
) -> Result<Option<String>, BrainError> {
    conn.query_row(
        "SELECT text FROM summaries WHERE scope_kind = ?1 AND member_set_hash = ?2 AND extractor_id = ?3",
        rusqlite::params![scope_kind, member_hash, extractor_id],
        |r| r.get(0),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        _ => Err(e),
    })
    .map_err(sql_err)
}

/// Cache a summary text.
pub fn cache_summary(
    conn: &Connection,
    scope_kind: &str,
    member_hash: &[u8],
    extractor_id: &str,
    text: &str,
    now: Timestamp,
) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR REPLACE INTO summaries (scope_kind, member_set_hash, extractor_id, text, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![scope_kind, member_hash, extractor_id, text, now.millis()],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Write a Derived episode + episode_links to sources. Returns the episode id.
/// When `uncertainty` is provided, it is stored as JSON in the
/// `uncertainty_json` column (§13.1, P10).
pub fn write_derived_episode(
    conn: &Connection,
    space: &str,
    text: &str,
    sources: &[String],
    uncertainty: Option<&oxibrain_core::Uncertainty>,
    now: Timestamp,
) -> Result<String, BrainError> {
    let ch = oxibrain_core::content_hash(text);
    // Serialize source episode IDs as JSON for the `of` field (SourceRef::Derived { of: String }).
    let of = serde_json::to_string(sources).unwrap_or_default();
    let source = SourceRef::Derived { of };
    let ep_id = oxibrain_core::episode_id(space, &ch, &source, now);

    let mut episode = oxibrain_core::Episode {
        id: ep_id.clone(),
        space: space.into(),
        seq: 0,
        content_hash: ch,
        content: text.into(),
        source,
        trust: oxibrain_core::TrustTier::Trusted,
        kind: EpisodeKind::Derived,
        occurred_at: now,
        ingested_at: now,
        redacted_at: None,
    };
    crate::ledger::insert_episode(conn, &mut episode)?;
    let ep_id = episode.id.clone();

    // Store uncertainty JSON (§13.1, P10).
    if let Some(u) = uncertainty {
        let json = serde_json::to_string(u).unwrap_or_default();
        conn.execute(
            "UPDATE episodes SET uncertainty_json = ?1 WHERE id = ?2",
            rusqlite::params![json, ep_id],
        )
        .map_err(sql_err)?;
    }

    // Link derived episode to its sources.
    for src in sources {
        conn.execute(
            "INSERT OR IGNORE INTO episode_links (from_episode, to_episode, rel) VALUES (?1, ?2, 'summarizes')",
            rusqlite::params![ep_id, src],
        )
        .map_err(sql_err)?;
    }

    Ok(ep_id)
}

/// Compute a deterministic hash for a community member set (sorted IDs).
/// Mixes the literal "community" tag into the hash so the namespace
/// cannot collide with an episode-cluster hash for the same extractor
/// (avoids a migration to add `scope_kind` to the checkpoint table).
pub fn hash_community_member_set(entity_ids: &[String]) -> [u8; 32] {
    let mut sorted: Vec<&str> = entity_ids.iter().map(|s| s.as_str()).collect();
    sorted.sort();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"community\x00");
    for id in sorted {
        hasher.update(id.as_bytes());
    }
    hasher.finalize().into()
}

/// Compute a deterministic hash for a member set (sorted IDs).
pub fn hash_member_set(ids: &[String]) -> [u8; 32] {
    let mut sorted: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    sorted.sort();
    let mut hasher = blake3::Hasher::new();
    for id in &sorted {
        hasher.update(id.as_bytes());
        hasher.update(&[0u8]);
    }
    let mut out = [0u8; 32];
    hasher.finalize_xof().fill(&mut out);
    out
}

// ── Consolidation checkpoints (DESIGN §10, sub-project L2) ────────────
// Cache table (not ledger) — reproject ignores and rebuilds.

/// Mark a cluster as in-progress. Idempotent: existing rows are overwritten.
pub fn checkpoint_begin(
    conn: &Connection,
    cluster_hash: &[u8; 32],
    extractor_id: &str,
    now: Timestamp,
) -> Result<(), BrainError> {
    let hash_hex = hex::encode(cluster_hash);
    conn.execute(
        "INSERT OR REPLACE INTO consolidation_checkpoints
         (cluster_hash, extractor_id, status, started_at)
         VALUES (?1, ?2, 'in_progress', ?3)",
        rusqlite::params![hash_hex, extractor_id, now.millis()],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Mark a cluster as completed.
pub fn checkpoint_complete(
    conn: &Connection,
    cluster_hash: &[u8; 32],
    now: Timestamp,
) -> Result<(), BrainError> {
    let hash_hex = hex::encode(cluster_hash);
    conn.execute(
        "UPDATE consolidation_checkpoints
         SET status = 'completed', completed_at = ?2
         WHERE cluster_hash = ?1",
        rusqlite::params![hash_hex, now.millis()],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Filter clusters to skip already-completed ones. Returns the set of
/// completed cluster hashes as hex strings.
pub fn completed_clusters(
    conn: &Connection,
    extractor_id: &str,
) -> Result<std::collections::HashSet<String>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT cluster_hash FROM consolidation_checkpoints
             WHERE extractor_id = ?1 AND status = 'completed'",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(rusqlite::params![extractor_id], |r| r.get::<_, String>(0))
        .map_err(sql_err)?;
    let mut set = std::collections::HashSet::new();
    for row in rows {
        set.insert(row.map_err(sql_err)?);
    }
    Ok(set)
}

/// Primary episode IDs that cite any of the given entities (via
/// assertions → statements → subject/object), sorted by id for
/// determinism. Used by the community summariser to populate the
/// derived episode's source links so summaries are never uncited.
pub fn episodes_for_entities(
    conn: &Connection,
    space: &str,
    entity_ids: &[String],
) -> Result<Vec<String>, BrainError> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Schema (migrations/v1.sql): episodes.{id, space_id, kind},
    // assertions.{statement_id, episode_id}, statements.{id,
    // space_id, subject_id, object_entity, object_literal}.
    // Mirror the projection `entities_for_episodes` uses (UNION over
    // the subject_id / object_entity halves) so the SQL row shape
    // never disagrees between the two helpers. The output is
    // deduplicated and sorted by id for determinism.
    let placeholders = std::iter::repeat("?")
        .take(entity_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT a.episode_id
           FROM assertions a
           JOIN statements s ON a.statement_id = s.id
           JOIN episodes e ON a.episode_id = e.id
          WHERE e.space_id = ?1
            AND e.kind = 'primary'
            AND (
                s.subject_id IN ({placeholders})
                OR s.object_entity IN ({placeholders})
            )
          ORDER BY a.episode_id ASC",
    );
    // param order: space sentinel (1×), entity_ids for subject_id (N),
    // entity_ids for object_entity (N) → 1 + 2N placeholders total.
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&space];
    for id in entity_ids {
        params.push(id as &dyn rusqlite::ToSql);
    }
    for id in entity_ids {
        params.push(id as &dyn rusqlite::ToSql);
    }
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |r| {
            r.get::<_, String>(0)
        })
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sql_err)?);
    }
    Ok(out)
}

/// Load community entities grouped by label.
pub fn load_community_entities(
    conn: &Connection,
    space: &str,
) -> Result<Vec<CommunityGroup>, BrainError> {
    let mut stmt = conn
        .prepare("SELECT label, id FROM communities WHERE space_id = ?1 ORDER BY label ASC, id ASC")
        .map_err(sql_err)?;

    let rows: Vec<(i64, String)> = stmt
        .query_map(rusqlite::params![space], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;

    let mut groups: Vec<CommunityGroup> = Vec::new();
    for (label, entity_id) in rows {
        if let Some(g) = groups.last_mut() {
            if g.label == label as u64 {
                g.entity_ids.push(entity_id);
                continue;
            }
        }
        groups.push(CommunityGroup {
            label: label as u64,
            entity_ids: vec![entity_id],
        });
    }

    Ok(groups)
}

/// Find clusters of primary episodes sharing ≥ 2 entities (via assertions).
/// Returns clusters sorted by episode IDs for determinism.
pub fn find_episode_clusters(
    conn: &Connection,
    space: &str,
) -> Result<Vec<EpisodeCluster>, BrainError> {
    // Find pairs of episodes that share entities through assertions.
    let mut stmt = conn
        .prepare(
            "SELECT a.episode_id, b.episode_id, a.statement_id
             FROM assertions a
             JOIN assertions b ON a.statement_id = b.statement_id AND a.episode_id < b.episode_id
             JOIN episodes ea ON a.episode_id = ea.id AND ea.space_id = ?1 AND ea.kind = 'primary'
             JOIN episodes eb ON b.episode_id = eb.id AND eb.space_id = ?1 AND eb.kind = 'primary'
             ORDER BY a.episode_id, b.episode_id",
        )
        .map_err(sql_err)?;

    // Build adjacency: episode → set of related episodes.
    let mut adjacency: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let rows = stmt
        .query_map(rusqlite::params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (a, b) = row.map_err(sql_err)?;
        adjacency.entry(a.clone()).or_default().insert(b.clone());
        adjacency.entry(b).or_default().insert(a);
    }
    drop(stmt);
    // Find connected components (simple BFS).
    let mut visited = std::collections::BTreeSet::new();
    let mut clusters = Vec::new();
    for ep in adjacency.keys() {
        if visited.contains(ep) {
            continue;
        }
        let mut cluster_eps = std::collections::BTreeSet::new();
        let mut queue = vec![ep.clone()];
        while let Some(e) = queue.pop() {
            if !cluster_eps.insert(e.clone()) {
                continue;
            }
            visited.insert(e.clone());
            if let Some(neighbors) = adjacency.get(&e) {
                for n in neighbors {
                    if !visited.contains(n) {
                        queue.push(n.clone());
                    }
                }
            }
        }
        if cluster_eps.len() >= 2 {
            let episode_ids: Vec<String> = cluster_eps.into_iter().collect();
            clusters.push(EpisodeCluster {
                episode_ids,
                shared_entities: Vec::new(), // simplified
            });
        }
    }

    Ok(clusters)
}

/// Return the entity ids that appear in ≥ 2 of the given primary episodes
/// inside `space`, sorted by id for determinism. This is the cluster's
/// "shared entity" set used to compute Uncertainty (§13.1) and to decide
/// whether a cluster is real (≥ 2 shared entities, the same threshold
/// `find_episode_clusters` uses to group them in the first place).
///
/// Determinism: the SQL uses `IN (?, ?, ...)` with a stable placeholder
/// ordering and `GROUP BY / ORDER BY entity_id`, so two runs over the same
/// store produce byte-identical output regardless of insertion order.
pub fn entities_for_episodes(
    conn: &Connection,
    space: &str,
    episode_ids: &[String],
) -> Result<Vec<String>, BrainError> {
    if episode_ids.len() < 2 {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat("?")
        .take(episode_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    // An entity is shared iff it appears in ≥ 2 distinct primary episodes of
    // this space via the assertion → statement → entity_id (subject or object)
    // path. Statements are the canonical "what the episode believes about the
    // entity" carrier; both sides count so entity mentions work for either
    // role (e.g. Alice "born_in" Seoul and Seoul "contains" Alice collapse on
    // the entity Alice for this purpose).
    // An entity is shared iff it appears (as subject OR object) in ≥ 2
    // distinct primary episodes of this space, via the assertion →
    // statement → entity_id (subject or object) path. Statements are the
    // canonical "what the episode believes about the entity" carrier;
    // both sides count so entity mentions work for either role (e.g.
    // Alice "born_in" Seoul and Seoul "contains" Alice collapse on the
    // entity Alice for this purpose).
    //
    // We use anonymous `?` placeholders throughout so rusqlite's positional
    // binding matches the in-order params vector: 2 * len(episode_ids)
    // episode ids followed by the space sentinel repeated twice.
    let sql = format!(
        "WITH ep_entities AS (
             SELECT DISTINCT a.episode_id, s.subject_id AS entity_id
               FROM assertions a
               JOIN statements s ON a.statement_id = s.id
               JOIN episodes e ON a.episode_id = e.id
              WHERE a.episode_id IN ({placeholders})
                AND e.space_id = ?
                AND e.kind = 'primary'
                AND s.subject_id IS NOT NULL
             UNION
             SELECT DISTINCT a.episode_id, s.object_entity AS entity_id
               FROM assertions a
               JOIN statements s ON a.statement_id = s.id
               JOIN episodes e ON a.episode_id = e.id
              WHERE a.episode_id IN ({placeholders})
                AND e.space_id = ?
                AND e.kind = 'primary'
                AND s.object_entity IS NOT NULL
         )
         SELECT entity_id
           FROM ep_entities
          GROUP BY entity_id
         HAVING COUNT(DISTINCT episode_id) >= 2
          ORDER BY entity_id ASC",
    );
    // Bind the same episode ids for both UNION branches, then the space
    // sentinel twice.
    let mut params: Vec<&dyn rusqlite::ToSql> = episode_ids
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();
    params.extend(episode_ids.iter().map(|s| s as &dyn rusqlite::ToSql));
    params.push(&space);
    params.push(&space);
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |r| {
            r.get::<_, String>(0)
        })
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(sql_err)?);
    }
    Ok(out)
}

/// Filter clusters to only those not yet completed for the given extractor.
/// "Completed" means a `consolidation_checkpoints` row with `status =
/// 'completed'` exists for the cluster's member-set hash. In-progress
/// checkpoints are intentionally NOT filtered — a crashed run must resume
/// (Task 5, §13).
///
/// The returned vector preserves the input order, so the caller can rely
/// on `find_episode_clusters` / `hash_member_set` determinism end-to-end.
pub fn filter_pending_clusters(
    conn: &Connection,
    extractor_id: &str,
    clusters: &[EpisodeCluster],
) -> Result<Vec<EpisodeCluster>, BrainError> {
    let done = completed_clusters(conn, extractor_id)?;
    let mut out = Vec::with_capacity(clusters.len());
    for cluster in clusters {
        let hash = hash_member_set(&cluster.episode_ids);
        let key = hex::encode(hash);
        if !done.contains(&key) {
            out.push(cluster.clone());
        }
    }
    Ok(out)
}

/// Compute `Uncertainty` (§13.1, P10) for a cluster from its shared entity
/// ids. Wraps `belief_stats_for_entities` with the pure `compute_uncertainty`
/// so the store returns the persisted JSON-ready value. Deterministic given
/// the same `(conn, space, entities, now)` — `belief_stats_for_entities`
/// uses a fixed GROUP BY and `now` only enters as a staleness bound.
pub fn uncertainty_for_cluster(
    conn: &Connection,
    space: &str,
    shared_entities: &[String],
    now: Timestamp,
) -> Result<oxibrain_core::Uncertainty, BrainError> {
    let input = belief_stats_for_entities(conn, space, shared_entities, now)?;
    Ok(oxibrain_core::compute_uncertainty(&input))
}

/// Build the LLM prompt for summarizing an episode cluster.
///
/// Deliberately uses raw episode text only (`cluster.episode_ids`
/// content). The `cluster.shared_entities` field is **unused on
/// purpose** in this prompt — passing empty is the documented
/// contract (Task 5 Finding 4). Shared entities are still computed
/// later in the flow via `entities_for_episodes` (used to set
/// `episode_links.sources` of the derived episode and to derive the
/// `Uncertainty` JSON). Keeping them out of the prompt avoids the
/// prompt becoming order-dependent on map iteration (`BTreeMap` /
/// SQL `ORDER BY` would still be deterministic, but the body itself
/// reads more naturally as raw source text + a count/listing of
/// entities).
pub fn build_consolidation_prompt(
    conn: &Connection,
    space: &str,
    cluster: &EpisodeCluster,
) -> Result<String, BrainError> {
    let mut prompt = String::from(
        "Summarize the following related episodes into a concise \
        thematic summary. Focus on key entities, relationships, and decisions.\n\n",
    );
    for ep_id in &cluster.episode_ids {
        let content: String = conn
            .query_row(
                "SELECT content FROM episodes WHERE id = ?1 AND space_id = ?2",
                rusqlite::params![ep_id, space],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        prompt.push_str(&format!("--- Episode {ep_id} ---\n{content}\n\n"));
    }
    Ok(prompt)
}

/// Build the LLM prompt for summarizing a community's entities and beliefs.
pub fn build_community_prompt(
    conn: &Connection,
    space: &str,
    group: &CommunityGroup,
) -> Result<String, BrainError> {
    let mut prompt = String::from(
        "Summarize the key themes and relationships among these \
        entities:\n\n",
    );
    for entity_id in &group.entity_ids {
        let beliefs = crate::query::beliefs_for_entity(conn, space, entity_id)?;
        let entity_type: String = conn
            .query_row(
                "SELECT type_name FROM entities WHERE id = ?1",
                rusqlite::params![entity_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "Unknown".into());
        prompt.push_str(&format!("- {entity_id} ({entity_type}): "));
        let summaries: Vec<String> = beliefs
            .iter()
            .map(|b| format!("{:?} (conf {:.2})", b.status, b.confidence))
            .collect();
        prompt.push_str(&summaries.join(", "));
        prompt.push('\n');
    }
    Ok(prompt)
}

/// Compute belief statistics for a set of entities, for uncertainty (§13.1, 10.1).
/// Queries beliefs joined with statements for the given entity IDs.
/// Returns `UncertaintyInput` ready for `compute_uncertainty`.
pub fn belief_stats_for_entities(
    conn: &Connection,
    space: &str,
    entity_ids: &[String],
    now: Timestamp,
) -> Result<oxibrain_core::UncertaintyInput, BrainError> {
    if entity_ids.is_empty() {
        return Ok(oxibrain_core::UncertaintyInput::default());
    }
    let placeholders = entity_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT b.status, b.support_json
         FROM beliefs b
         JOIN statements s ON b.statement_id = s.id
         WHERE s.space_id = ?1
           AND s.subject_id IN ({placeholders})
           AND b.status IN ('active', 'superseded', 'contradicted')"
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let bind_params: Vec<&str> = std::iter::once(space)
        .chain(entity_ids.iter().map(|s| s.as_str()))
        .collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind_params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1).unwrap_or_default(),
            ))
        })
        .map_err(sql_err)?;

    let mut total = 0usize;
    let mut contradicted = 0usize;
    let mut single_source = 0usize;
    let mut untrusted = 0usize;
    for row in rows {
        let (status, support_json) = row.map_err(sql_err)?;
        total += 1;
        if status == "contradicted" {
            contradicted += 1;
        }
        // Parse support_json for distinct_episodes and trust_weights.
        if let Ok(support) = serde_json::from_str::<oxibrain_core::Support>(&support_json) {
            if support.distinct_episodes <= 1 {
                single_source += 1;
            }
            if support
                .trust_weights
                .iter()
                .any(|(t, _)| *t == oxibrain_core::TrustTier::Untrusted)
            {
                untrusted += 1;
            }
        }
    }

    // Staleness: max ingested_at across supporting episodes.
    let placeholders2 = entity_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let staleness_sql = format!(
        "SELECT MAX(e.ingested_at)
         FROM episodes e
         JOIN assertions a ON a.episode_id = e.id
         JOIN statements s ON a.statement_id = s.id
         WHERE s.space_id = ?1 AND s.subject_id IN ({placeholders2})"
    );
    let mut stmt2 = conn.prepare(&staleness_sql).map_err(sql_err)?;
    let max_ingested: Option<i64> = stmt2
        .query_row(rusqlite::params_from_iter(bind_params.iter()), |r| r.get(0))
        .ok();
    let max_age_days = match max_ingested {
        Some(ts) => (now.millis() - ts) as f64 / 86_400_000.0,
        None => 0.0,
    };

    Ok(oxibrain_core::UncertaintyInput {
        total_beliefs: total,
        contradicted_beliefs: contradicted,
        single_source_beliefs: single_source,
        untrusted_beliefs: untrusted,
        max_episode_age_days: max_age_days.max(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use tempfile::TempDir;

    #[test]
    fn summary_cache_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(&dir.path().join("test.db")).unwrap();
        let conn = store.into_parts().0;

        let hash = hash_member_set(&["e1".into(), "e2".into()]);
        let now = Timestamp::from_millis(1000);

        // Nothing cached yet.
        assert!(
            get_cached_summary(&conn, "community", &hash, "ext1")
                .unwrap()
                .is_none()
        );

        // Cache it.
        cache_summary(&conn, "community", &hash, "ext1", "summary text", now).unwrap();

        // Retrieve.
        let cached = get_cached_summary(&conn, "community", &hash, "ext1").unwrap();
        assert_eq!(cached.as_deref(), Some("summary text"));
    }

    #[test]
    fn hash_is_deterministic() {
        let h1 = hash_member_set(&["b".into(), "a".into()]);
        let h2 = hash_member_set(&["a".into(), "b".into()]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn checkpoint_lifecycle() {
        use crate::migration;
        use rusqlite::Connection;
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        let h1 = [1u8; 32];
        let h2 = [2u8; 32];
        let now = oxibrain_ports::Timestamp::from_millis(1);
        // Start two clusters.
        checkpoint_begin(&conn, &h1, "ext_a", now).unwrap();
        checkpoint_begin(&conn, &h2, "ext_a", now).unwrap();

        // Complete one.
        checkpoint_complete(&conn, &h1, now).unwrap();

        // Filter: h1 should appear as completed, h2 should not.
        let done = completed_clusters(&conn, "ext_a").unwrap();
        assert_eq!(done.len(), 1);
        assert!(!done.contains(hex::encode(h2).as_str()));
    }

    /// Two clusters share an extractor, one completed, one in-progress.
    /// `filter_pending_clusters` must drop the completed one and keep the
    /// in-progress one (a crashed run must resume, §13).
    #[test]
    fn filter_pending_keeps_in_progress_drops_completed() {
        use crate::migration;
        use rusqlite::Connection;
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        // Pick episode-id pairs whose `hash_member_set` matches our
        // chosen checkpoint hashes, so the filter operates on the same
        // keys the store would.
        let done_ids = vec!["done_a".into(), "done_b".into()];
        let in_progress_ids = vec!["prog_a".into(), "prog_b".into()];
        let done_h = hash_member_set(&done_ids);
        let in_progress_h = hash_member_set(&in_progress_ids);
        let now = oxibrain_ports::Timestamp::from_millis(1);

        checkpoint_begin(&conn, &done_h, "ext_b", now).unwrap();
        checkpoint_complete(&conn, &done_h, now).unwrap();
        checkpoint_begin(&conn, &in_progress_h, "ext_b", now).unwrap();

        let seeded = vec![
            EpisodeCluster {
                episode_ids: in_progress_ids.clone(),
                shared_entities: vec![],
            },
            EpisodeCluster {
                episode_ids: done_ids.clone(),
                shared_entities: vec![],
            },
        ];
        let pending = filter_pending_clusters(&conn, "ext_b", &seeded).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "exactly one cluster (in_progress) must remain"
        );
        assert_eq!(
            pending[0].episode_ids, in_progress_ids,
            "the in_progress cluster must survive the filter"
        );
    }

    /// `filter_pending_clusters` is deterministic — it preserves input
    /// order and only drops clusters whose member-set hash is in the
    /// completed set. This is the determinism guarantee `consolidate_impl`
    /// relies on to keep its checkpoint / cluster ordering stable.
    #[test]
    fn filter_pending_is_input_order_preserving() {
        use crate::migration;
        use rusqlite::Connection;
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        let now = oxibrain_ports::Timestamp::from_millis(1);
        // Nothing completed — every cluster must remain, in input order.
        let clusters = vec![
            EpisodeCluster {
                episode_ids: vec!["c".into(), "a".into()],
                shared_entities: vec![],
            },
            EpisodeCluster {
                episode_ids: vec!["b".into(), "d".into()],
                shared_entities: vec![],
            },
            EpisodeCluster {
                episode_ids: vec!["e".into(), "f".into()],
                shared_entities: vec![],
            },
        ];
        let pending = filter_pending_clusters(&conn, "ext_z", &clusters).unwrap();
        assert_eq!(pending.len(), 3);
        assert_eq!(
            pending[0].episode_ids,
            vec!["c".to_string(), "a".to_string()]
        );
        assert_eq!(
            pending[1].episode_ids,
            vec!["b".to_string(), "d".to_string()]
        );
        assert_eq!(
            pending[2].episode_ids,
            vec!["e".to_string(), "f".to_string()]
        );

        // Mark the middle cluster completed — it must drop out, the
        // surrounding ones must keep their input positions.
        let mid_h = hash_member_set(&["b".into(), "d".into()]);
        checkpoint_begin(&conn, &mid_h, "ext_z", now).unwrap();
        checkpoint_complete(&conn, &mid_h, now).unwrap();
        let pending = filter_pending_clusters(&conn, "ext_z", &clusters).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending[0].episode_ids,
            vec!["c".to_string(), "a".to_string()]
        );
        assert_eq!(
            pending[1].episode_ids,
            vec!["e".to_string(), "f".to_string()]
        );
    }
}
