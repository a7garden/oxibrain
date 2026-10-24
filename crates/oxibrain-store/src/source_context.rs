//! Mechanical fetches of the beginning of selected raw sources.
use crate::{read_evidence::SourceReadScope, sql_err};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct RawSourcePrefix {
    pub episode: String,
    pub bytes: Vec<u8>,
    pub revision: String,
    pub source_kind: String,
    pub source_ref: Option<String>,
    pub occurred_at: Timestamp,
    pub trust: String,
}

#[derive(Debug, Clone, Default)]
pub struct PrefixFetch {
    pub prefixes: Vec<RawSourcePrefix>,
    /// Planned IDs on which the store query was attempted.
    pub considered: usize,
    pub bytes_read: usize,
    pub capped: bool,
    pub available: bool,
}

/// Fetch bounded ordinal-zero bytes for selected episodes. The query
/// repeats every source visibility predicate used by retrieval, so stale or
/// cross-scope IDs cannot expose source text.
pub fn fetch_prefixes(
    conn: &Connection,
    scope: &SourceReadScope<'_>,
    episodes: &[String],
    source_context_bytes: usize,
    channel_depth: usize,
    materialization_bytes: usize,
) -> Result<PrefixFetch, BrainError> {
    if episodes.is_empty() || source_context_bytes == 0 || materialization_bytes == 0 {
        return Ok(PrefixFetch {
            available: true,
            ..Default::default()
        });
    }
    let available: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_source_chunks')",
            [],
            |row| row.get(0),
        )
        .map_err(sql_err)?;
    if !available {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(sql_err)?;
        if version >= 15 {
            return Err(BrainError::Corruption(
                "schema 15 is missing its raw-source index".into(),
            ));
        }
        return Ok(PrefixFetch {
            available: false,
            ..Default::default()
        });
    }

    let byte_limit = source_context_bytes.min(materialization_bytes);
    let count_limit = channel_depth.min(materialization_bytes / byte_limit);
    let considered = episodes.len().min(count_limit);
    let mut result = PrefixFetch {
        considered,
        capped: episodes.len() > considered,
        available: true,
        ..Default::default()
    };
    if considered == 0 {
        return Ok(result);
    }

    let byte_limit = i64::try_from(byte_limit).map_err(|_| {
        BrainError::Invalid("source context byte limit exceeds SQLite range".into())
    })?;
    let body = "CASE WHEN e.content='' THEN CAST(coalesce(e.content_compacted,'') AS BLOB) ELSE CAST(e.content AS BLOB) END";
    let sql = format!(
        "SELECT e.id,substr({body},c.span_start+1,min(c.span_end-c.span_start,?5)),hex(e.content_hash),e.source_kind,e.source_ref,e.occurred_at,e.trust
         FROM memory_source_chunks c JOIN episodes e ON e.id=c.episode_id
         WHERE c.episode_id=?6 AND c.space_id=?1 AND c.ordinal=0 AND c.span_start=0
         AND e.space_id=?1 AND e.kind='primary' AND e.redacted_at IS NULL
         AND e.source_kind NOT IN ('document','document_revision')
         AND e.ingested_at<=?2 AND e.occurred_at<=?3
         AND (?4 IS NULL OR e.source_kind!='session' OR json_extract(e.source_ref,'$.session_id') IS NOT ?4)"
    );
    let mut statement = conn.prepare(&sql).map_err(sql_err)?;
    for episode in episodes.iter().take(considered) {
        let row = statement.query_row(
            params![
                scope.space,
                scope.known_at.0,
                scope.as_of.0,
                scope.excluded_session,
                byte_limit,
                episode
            ],
            |row| {
                Ok(RawSourcePrefix {
                    episode: row.get(0)?,
                    bytes: row.get(1)?,
                    revision: row.get(2)?,
                    source_kind: row.get(3)?,
                    source_ref: row.get(4)?,
                    occurred_at: Timestamp(row.get(5)?),
                    trust: row.get(6)?,
                })
            },
        );
        match row {
            Ok(prefix) => {
                result.bytes_read += prefix.bytes.len();
                result.prefixes.push(prefix);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => return Err(sql_err(error)),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE episodes(
                id TEXT PRIMARY KEY, space_id TEXT NOT NULL, kind TEXT NOT NULL,
                source_kind TEXT NOT NULL, source_ref TEXT, redacted_at INTEGER,
                ingested_at INTEGER NOT NULL, occurred_at INTEGER NOT NULL,
                content TEXT NOT NULL, content_compacted TEXT, content_hash BLOB NOT NULL,
                trust TEXT NOT NULL
             );
             CREATE TABLE memory_source_chunks(
                id TEXT PRIMARY KEY, space_id TEXT NOT NULL, episode_id TEXT NOT NULL,
                ordinal INTEGER NOT NULL, span_start INTEGER NOT NULL, span_end INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The fixture exposes each independently varied source-scope column."
    )]
    fn add_source(
        conn: &Connection,
        id: &str,
        space: &str,
        kind: &str,
        source_kind: &str,
        source_ref: &str,
        redacted_at: Option<i64>,
        ingested_at: i64,
        occurred_at: i64,
        content: &str,
        chunk_ordinal: i64,
    ) {
        conn.execute(
            "INSERT INTO episodes(id,space_id,kind,source_kind,source_ref,redacted_at,ingested_at,occurred_at,content,content_hash,trust)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'untrusted')",
            params![
                id,
                space,
                kind,
                source_kind,
                source_ref,
                redacted_at,
                ingested_at,
                occurred_at,
                content,
                id.as_bytes()
            ],
        )
        .unwrap();
        let span_end = content.len();
        conn.execute(
            "INSERT INTO memory_source_chunks(id,space_id,episode_id,ordinal,span_start,span_end)
             VALUES(?1,?2,?3,?4,0,?5)",
            params![
                format!("chunk-{id}-{chunk_ordinal}"),
                space,
                id,
                chunk_ordinal,
                span_end
            ],
        )
        .unwrap();
    }

    #[test]
    fn fetch_is_scoped_to_live_primary_non_document_sources_and_ordinal_zero() {
        let conn = connection();
        add_source(
            &conn,
            "eligible",
            "inside",
            "primary",
            "session",
            r#"{"session_id":"past"}"#,
            None,
            10,
            20,
            "éarly source prefix",
            0,
        );
        add_source(
            &conn,
            "wrong-space",
            "outside",
            "primary",
            "session",
            r#"{"session_id":"past"}"#,
            None,
            10,
            20,
            "other space",
            0,
        );
        add_source(
            &conn,
            "future-known",
            "inside",
            "primary",
            "session",
            r#"{"session_id":"past"}"#,
            None,
            101,
            20,
            "not known yet",
            0,
        );
        add_source(
            &conn,
            "future-valid",
            "inside",
            "primary",
            "session",
            r#"{"session_id":"past"}"#,
            None,
            10,
            101,
            "not valid yet",
            0,
        );
        add_source(
            &conn,
            "excluded-session",
            "inside",
            "primary",
            "session",
            r#"{"session_id":"current"}"#,
            None,
            10,
            20,
            "current session",
            0,
        );
        add_source(
            &conn,
            "redacted",
            "inside",
            "primary",
            "session",
            r#"{"session_id":"past"}"#,
            Some(30),
            10,
            20,
            "redacted source",
            0,
        );
        add_source(
            &conn,
            "derived",
            "inside",
            "derived",
            "session",
            r#"{"session_id":"past"}"#,
            None,
            10,
            20,
            "derived source",
            0,
        );
        add_source(
            &conn,
            "document",
            "inside",
            "primary",
            "document",
            "{}",
            None,
            10,
            20,
            "document source",
            0,
        );
        add_source(
            &conn,
            "not-first-window",
            "inside",
            "primary",
            "session",
            r#"{"session_id":"past"}"#,
            None,
            10,
            20,
            "later window only",
            1,
        );

        let scope = SourceReadScope {
            space: "inside",
            known_at: Timestamp(100),
            as_of: Timestamp(100),
            excluded_session: Some("current"),
        };
        let ids = [
            "eligible",
            "wrong-space",
            "future-known",
            "future-valid",
            "excluded-session",
            "redacted",
            "derived",
            "document",
            "not-first-window",
        ]
        .map(str::to_string);
        let result = fetch_prefixes(&conn, &scope, &ids, 8, 16, 128).unwrap();

        assert!(result.available);
        assert_eq!(result.considered, ids.len());
        assert_eq!(result.prefixes.len(), 1);
        assert_eq!(result.prefixes[0].episode, "eligible");
        assert_eq!(result.prefixes[0].bytes, "éarly s".as_bytes());
        assert_eq!(
            result.prefixes[0].revision,
            hex::encode("eligible").to_uppercase()
        );
        assert_eq!(result.bytes_read, 8);
        assert!(!result.capped);
    }

    #[test]
    fn fetch_obeys_channel_depth_and_total_materialization_budget() {
        let conn = connection();
        let ids: Vec<_> = (0..3).map(|i| format!("source-{i}")).collect();
        for id in &ids {
            add_source(
                &conn,
                id,
                "space",
                "primary",
                "session",
                r#"{"session_id":"past"}"#,
                None,
                10,
                20,
                "0123456789",
                0,
            );
        }
        let scope = SourceReadScope {
            space: "space",
            known_at: Timestamp(100),
            as_of: Timestamp(100),
            excluded_session: None,
        };

        let channel_limited = fetch_prefixes(&conn, &scope, &ids, 4, 1, 100).unwrap();
        assert_eq!(channel_limited.considered, 1);
        assert_eq!(channel_limited.prefixes.len(), 1);
        assert_eq!(channel_limited.bytes_read, 4);
        assert!(channel_limited.capped);

        let bytes_limited = fetch_prefixes(&conn, &scope, &ids, 4, 8, 8).unwrap();
        assert_eq!(bytes_limited.considered, 2);
        assert_eq!(bytes_limited.prefixes.len(), 2);
        assert_eq!(bytes_limited.bytes_read, 8);
        assert!(bytes_limited.capped);
    }
}
