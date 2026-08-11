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
pub fn write_derived_episode(
    conn: &Connection,
    space: &str,
    text: &str,
    sources: &[String],
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
}
