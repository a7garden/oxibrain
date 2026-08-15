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

/// Build the LLM prompt for summarizing an episode cluster.
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
}
