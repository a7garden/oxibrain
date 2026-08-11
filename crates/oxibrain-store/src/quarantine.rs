//! Quarantine: extraction failure recording (DESIGN §7.4).
//! Invalid claims that exhaust the repair loop are filed here — never silently dropped.

use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionFailure {
    pub id: i64,
    pub episode_id: String,
    pub extractor_id: String,
    pub raw_response: String,
    pub errors_json: String,
    pub created_at: Timestamp,
}

/// Record an extraction failure (invalid claims that exhausted repair).
pub fn record_failure(
    conn: &Connection,
    episode_id: &str,
    extractor_id: &str,
    raw_response: &str,
    errors_json: &str,
    now: Timestamp,
) -> Result<i64, BrainError> {
    conn.execute(
        "INSERT INTO extraction_failures (episode_id, extractor_id, raw_response, errors_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![episode_id, extractor_id, raw_response, errors_json, now.millis()],
    )
    .map_err(sql_err)?;
    Ok(conn.last_insert_rowid())
}

/// List all extraction failures, optionally filtered by space (via episode join).
pub fn list_failures(
    conn: &Connection,
    space: Option<&str>,
) -> Result<Vec<ExtractionFailure>, BrainError> {
    let sql = if space.is_some() {
        "SELECT f.id, f.episode_id, f.extractor_id, f.raw_response, f.errors_json, f.created_at
         FROM extraction_failures f
         JOIN episodes e ON f.episode_id = e.id
         WHERE e.space_id = ?1
         ORDER BY f.created_at DESC"
    } else {
        "SELECT id, episode_id, extractor_id, raw_response, errors_json, created_at
         FROM extraction_failures
         ORDER BY created_at DESC"
    };

    let mut stmt = conn.prepare(sql).map_err(sql_err)?;
    let map_fn = |r: &rusqlite::Row| {
        Ok(ExtractionFailure {
            id: r.get(0)?,
            episode_id: r.get(1)?,
            extractor_id: r.get(2)?,
            raw_response: r.get(3)?,
            errors_json: r.get(4)?,
            created_at: Timestamp::from_millis(r.get(5)?),
        })
    };

    let failures = if let Some(s) = space {
        stmt.query_map(rusqlite::params![s], map_fn)
    } else {
        stmt.query_map([], map_fn)
    }
    .map_err(sql_err)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(sql_err)?;

    Ok(failures)
}
