//! Mechanical source-vector projection I/O. Core owns relevance and top-k.
use crate::{
    read_evidence::{RawSourceHit, SourceReadScope},
    sql_err,
};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

pub const WINDOW_POLICY: &str = "source-paragraph-window/2";
const ELIGIBLE: &str = "e.space_id=?1 AND e.kind='primary' AND e.redacted_at IS NULL AND e.source_kind NOT IN ('document','document_revision') AND e.ingested_at<=?2 AND e.occurred_at<=?3 AND (?4 IS NULL OR e.source_kind!='session' OR json_extract(e.source_ref,'$.session_id') IS NOT ?4)";
const BODY: &str = "CAST(substr(CASE WHEN e.content='' THEN e.content_compacted ELSE CAST(e.content AS BLOB) END,c.span_start+1,c.span_end-c.span_start) AS TEXT)";

#[derive(Debug, Clone)]
pub struct PendingVector {
    pub id: String,
    pub revision: String,
    pub text: String,
}
#[derive(Debug, Clone)]
pub struct VectorRow {
    pub id: String,
    pub vector: Vec<f32>,
}

pub fn available(conn: &Connection) -> Result<bool, BrainError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='memory_source_vectors' AND type='table')", [], |r| r.get(0)).map_err(sql_err)
}

pub fn pending(
    conn: &Connection,
    space: &str,
    model: &str,
    limit: usize,
) -> Result<Vec<PendingVector>, BrainError> {
    let sql = format!(
        "SELECT c.id,hex(e.content_hash),{BODY} FROM memory_source_chunks c JOIN episodes e ON e.id=c.episode_id WHERE e.space_id=?1 AND e.kind='primary' AND e.redacted_at IS NULL AND e.source_kind NOT IN ('document','document_revision') AND NOT EXISTS(SELECT 1 FROM memory_source_vectors v WHERE v.chunk_id=c.id AND v.model_identity=?2 AND v.revision=hex(e.content_hash) AND v.window_policy=?3) ORDER BY c.id LIMIT ?4"
    );
    let mut q = conn.prepare(&sql).map_err(sql_err)?;
    q.query_map(params![space, model, WINDOW_POLICY, limit], |r| {
        Ok(PendingVector {
            id: r.get(0)?,
            revision: r.get(1)?,
            text: r.get(2)?,
        })
    })
    .map_err(sql_err)?
    .collect::<Result<_, _>>()
    .map_err(sql_err)
}

/// CAS against the current source occurrence/revision. Inference has already finished.
pub fn upsert(
    conn: &Connection,
    model: &str,
    rows: &[(PendingVector, Vec<f32>)],
) -> Result<usize, BrainError> {
    let mut written = 0;
    for (source, vector) in rows {
        if vector.is_empty() || vector.iter().any(|v| !v.is_finite()) {
            return Err(BrainError::Invalid("invalid source embedding".into()));
        }
        let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        written += conn.execute("INSERT OR REPLACE INTO memory_source_vectors(chunk_id,revision,window_policy,model_identity,dimensions,embedding) SELECT c.id,?2,?3,?4,?5,?6 FROM memory_source_chunks c JOIN episodes e ON e.id=c.episode_id WHERE c.id=?1 AND hex(e.content_hash)=?2 AND e.redacted_at IS NULL AND e.kind='primary'", params![source.id,source.revision,WINDOW_POLICY,model,vector.len(),blob]).map_err(sql_err)?;
    }
    Ok(written)
}

pub fn coverage(
    conn: &Connection,
    scope: &SourceReadScope<'_>,
    model: &str,
) -> Result<(usize, usize), BrainError> {
    let sql = format!(
        "SELECT count(*),coalesce(sum(CASE WHEN v.chunk_id IS NOT NULL THEN 1 ELSE 0 END),0) FROM memory_source_chunks c JOIN episodes e ON e.id=c.episode_id LEFT JOIN memory_source_vectors v ON v.chunk_id=c.id AND v.revision=hex(e.content_hash) AND v.window_policy=?5 AND v.model_identity=?6 WHERE {ELIGIBLE}"
    );
    conn.query_row(
        &sql,
        params![
            scope.space,
            scope.known_at.0,
            scope.as_of.0,
            scope.excluded_session,
            WINDOW_POLICY,
            model
        ],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(sql_err)
}

pub fn batch(
    conn: &Connection,
    scope: &SourceReadScope<'_>,
    model: &str,
    after: &str,
    limit: usize,
) -> Result<Vec<VectorRow>, BrainError> {
    let sql = format!(
        "SELECT c.id,v.dimensions,v.embedding FROM memory_source_chunks c JOIN episodes e ON e.id=c.episode_id JOIN memory_source_vectors v ON v.chunk_id=c.id AND v.revision=hex(e.content_hash) AND v.window_policy=?5 AND v.model_identity=?6 WHERE {ELIGIBLE} AND c.id>?7 ORDER BY c.id LIMIT ?8"
    );
    let mut q = conn.prepare(&sql).map_err(sql_err)?;
    let rows = q
        .query_map(
            params![
                scope.space,
                scope.known_at.0,
                scope.as_of.0,
                scope.excluded_session,
                WINDOW_POLICY,
                model,
                after,
                limit
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, usize>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, dimensions, blob) = row.map_err(sql_err)?;
        if dimensions == 0 || dimensions.checked_mul(4) != Some(blob.len()) {
            return Err(BrainError::Corruption(
                "invalid source vector dimensions".into(),
            ));
        }
        let vector: Vec<_> = blob
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f32::from_le_bytes(*v))
            .collect();
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(BrainError::Corruption("non-finite source vector".into()));
        }
        out.push(VectorRow { id, vector });
    }
    Ok(out)
}

/// Materialize only selected windows, with the same visibility predicates as scoring.
pub fn hits(
    conn: &Connection,
    scope: &SourceReadScope<'_>,
    ids: &[String],
) -> Result<Vec<RawSourceHit>, BrainError> {
    let sql = format!(
        "SELECT c.id,e.id,{BODY},c.span_start,c.span_end,hex(e.content_hash),e.source_kind,e.source_ref,e.ingested_at,e.occurred_at,e.trust FROM memory_source_chunks c JOIN episodes e ON e.id=c.episode_id WHERE {ELIGIBLE} AND c.id=?5"
    );
    let mut q = conn.prepare(&sql).map_err(sql_err)?;
    let mut out = Vec::new();
    for id in ids {
        let row = q.query_row(
            params![
                scope.space,
                scope.known_at.0,
                scope.as_of.0,
                scope.excluded_session,
                id
            ],
            |r| {
                Ok(RawSourceHit {
                    id: r.get(0)?,
                    episode: r.get(1)?,
                    content: r.get(2)?,
                    start: r.get(3)?,
                    end: r.get(4)?,
                    revision: r.get(5)?,
                    source_kind: r.get(6)?,
                    source_ref: r.get(7)?,
                    ingested_at: Timestamp(r.get(8)?),
                    occurred_at: Timestamp(r.get(9)?),
                    trust: r.get(10)?,
                })
            },
        );
        match row {
            Ok(row) => out.push(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(sql_err(e)),
        }
    }
    Ok(out)
}
