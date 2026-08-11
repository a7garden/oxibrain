//! Lifecycle WriteOps: salience decay + compaction (DESIGN §10).

use crate::sql_err;
use oxibrain_core::lifecycle::{salience, DecayConfig};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

/// Recalculate salience for all entities in a space using time-decay formula.
/// Returns the number of entities updated.
pub fn apply_decay(
    conn: &Connection,
    space: &str,
    now: Timestamp,
    config: &DecayConfig,
) -> Result<usize, BrainError> {
    // Read all entities with their last_activity.
    let mut stmt = conn
        .prepare("SELECT id, last_activity FROM entities WHERE space_id = ?1")
        .map_err(sql_err)?;
    let entities: Vec<(String, Option<i64>)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);

    let mut count = 0;
    for (entity_id, last_activity) in &entities {
        let last = last_activity.map(Timestamp).unwrap_or(now);
        let salience_val = salience(last, now, config);
        conn.execute(
            "UPDATE entities SET salience = ?1 WHERE id = ?2",
            params![salience_val, entity_id],
        )
        .map_err(sql_err)?;
        count += 1;
    }
    Ok(count)
}

/// Compact cold episodes: move their content into the `content_compacted` BLOB
/// and clear the in-line `content` column. Returns the number of episodes
/// compacted. M2 default: store raw bytes in the BLOB (no compression yet —
/// the interface is what matters; can layer `flate2` later without breaking
/// callers).
pub fn compact_episodes(
    conn: &Connection,
    space: &str,
    now: Timestamp,
    min_age_days: u32,
) -> Result<usize, BrainError> {
    let min_age_millis = (min_age_days as i64) * 86_400_000;
    let cutoff = now.millis() - min_age_millis;
    // Find episodes older than cutoff that haven't been compacted.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
             WHERE space_id = ?1
               AND compacted_at IS NULL
               AND ingested_at < ?2
               AND content != ''",
        )
        .map_err(sql_err)?;
    let candidates: Vec<(String, String)> = stmt
        .query_map(params![space, cutoff], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);

    let mut count = 0;
    for (id, content) in &candidates {
        let compressed = content.as_bytes().to_vec();
        conn.execute(
            "UPDATE episodes SET content_compacted = ?1, compacted_at = ?2, content = ''
             WHERE id = ?3",
            params![compressed, now.millis(), id],
        )
        .map_err(sql_err)?;
        count += 1;
    }
    Ok(count)
}
