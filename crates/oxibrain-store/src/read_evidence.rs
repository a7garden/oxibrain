//! Mechanical source indexing/fetches. Visibility, fusion and selection belong to core.
use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

pub fn index_source(
    conn: &Connection,
    space: &str,
    episode: &str,
    body: &str,
) -> Result<(), BrainError> {
    // Historical schema fixtures and pre-v15 migration stages have no raw
    // index capability. The v15 backfill owns those existing sources.
    if !raw_index_available(conn)? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM memory_source_chunks WHERE episode_id=?1",
        [episode],
    )
    .map_err(sql_err)?;
    for c in oxibrain_core::chunking::source_windows(body, 2048, 256) {
        let id = format!("raw-{}", oxibrain_core::chunk_id(episode, c.ordinal));
        conn.execute("INSERT INTO memory_source_chunks(id,space_id,episode_id,ordinal,span_start,span_end) VALUES(?1,?2,?3,?4,?5,?6)",
            params![id,space,episode,c.ordinal,c.span_start,c.span_end]).map_err(sql_err)?;
        let rowid = conn.last_insert_rowid();
        for table in ["memory_chunk_fts_word", "memory_chunk_fts_ngram"] {
            conn.execute(
                &format!("INSERT INTO {table}(rowid,body) VALUES(?1,?2)"),
                params![rowid, &body[c.span_start..c.span_end]],
            )
            .map_err(sql_err)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct RawSourceHit {
    pub id: String,
    pub episode: String,
    pub content: String,
    pub start: usize,
    pub end: usize,
    pub revision: String,
    pub source_kind: String,
    pub source_ref: Option<String>,
    pub ingested_at: Timestamp,
    pub occurred_at: Timestamp,
    pub trust: String,
}

pub struct SourceReadScope<'a> {
    pub space: &'a str,
    pub known_at: Timestamp,
    pub as_of: Timestamp,
    pub excluded_session: Option<&'a str>,
}
pub fn source_channel(
    conn: &Connection,
    scope: &SourceReadScope<'_>,
    term: &str,
    ngram: bool,
    limit: usize,
) -> Result<Vec<RawSourceHit>, BrainError> {
    let SourceReadScope {
        space,
        known_at,
        as_of,
        excluded_session,
    } = scope;
    if term.is_empty() {
        return Ok(Vec::new());
    }
    let table = if ngram {
        "memory_chunk_fts_ngram"
    } else {
        "memory_chunk_fts_word"
    };
    let sql = format!("SELECT c.id,e.id,CAST(substr(CASE WHEN e.content='' THEN e.content_compacted ELSE CAST(e.content AS BLOB) END,c.span_start+1,c.span_end-c.span_start) AS TEXT),c.span_start,c.span_end,hex(e.content_hash),e.source_kind,e.source_ref,e.ingested_at,e.occurred_at,e.trust
        FROM {table} f JOIN memory_source_chunks c ON c.rowid=f.rowid JOIN episodes e ON e.id=c.episode_id
        WHERE {table} MATCH ?1 AND e.space_id=?2 AND e.redacted_at IS NULL
        AND e.kind='primary' AND e.source_kind NOT IN ('document','document_revision')
        AND e.ingested_at<=?3 AND e.occurred_at<=?4
        AND (?5 IS NULL OR e.source_kind!='session' OR json_extract(e.source_ref,'$.session_id') IS NOT ?5)
        ORDER BY f.rank,c.id LIMIT ?6");
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let quoted = format!("\"{}\"", term.replace('"', "\"\""));
    let rows = stmt
        .query_map(
            params![quoted, space, known_at.0, as_of.0, excluded_session, limit],
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
        )
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

/// Canonical-sequence batch; the caller commits rows and cursor in one transaction.
pub fn source_batch(
    conn: &Connection,
    space: &str,
    after: i64,
    target: i64,
    limit: usize,
) -> Result<Vec<(i64, String, String)>, BrainError> {
    let mut s=conn.prepare("SELECT seq,id,CASE WHEN content='' THEN coalesce(CAST(content_compacted AS TEXT),'') ELSE content END FROM episodes WHERE space_id=?1 AND seq>?2 AND seq<=?3 AND kind='primary' AND redacted_at IS NULL AND source_kind NOT IN ('document','document_revision') ORDER BY seq LIMIT ?4").map_err(sql_err)?;
    s.query_map(params![space, after, target, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .map_err(sql_err)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(sql_err)
}

pub fn frontier(conn: &Connection, space: &str) -> Result<i64, BrainError> {
    conn.query_row(
        "SELECT coalesce(max(seq),0) FROM episodes WHERE space_id=?1",
        [space],
        |r| r.get(0),
    )
    .map_err(sql_err)
}

pub fn rebuild_raw(conn: &Connection, space: &str) -> Result<(), BrainError> {
    if !raw_index_available(conn)? {
        return Ok(());
    }
    let target = frontier(conn, space)?;
    let mut after = -1;
    loop {
        let batch = source_batch(conn, space, after, target, 64)?;
        if batch.is_empty() {
            break;
        }
        for (seq, id, body) in batch {
            index_source(conn, space, &id, &body)?;
            after = seq;
        }
    }
    Ok(())
}

/// Complete groups only. The aggregate size guard precedes materialization.
pub struct TruthInput {
    pub groups: Vec<Vec<oxibrain_core::fold::StatementEntry>>,
    pub definitions: std::collections::BTreeMap<String, oxibrain_core::registry::PredicateDef>,
    pub live_sources: std::collections::BTreeSet<String>,
    pub surfaces: std::collections::BTreeMap<String, String>,
    pub entity_types: std::collections::BTreeMap<String, String>,
    pub omitted_groups: usize,
    pub merges: Vec<oxibrain_core::EntityMerge>,
    pub merge_surfaces: std::collections::BTreeMap<String, String>,
}

pub fn truth_input(
    conn: &Connection,
    space: &str,
    known_at: Timestamp,
    selected: &[oxibrain_core::truth_selection::GroupObservation],
) -> Result<TruthInput, BrainError> {
    let known_at = known_at.0;
    let mut definitions: std::collections::BTreeMap<_, _> = oxibrain_core::registry::core_v1()
        .iter()
        .map(|d| (d.name.clone(), d.clone()))
        .collect();
    // Registry updates are ledger declarations. A future replacement of a
    // predicate definition cannot change the meaning of an earlier read.
    let mut declarations=conn.prepare("SELECT json_extract(content,'$.name'),json_extract(content,'$.def_json') FROM episodes WHERE space_id=?1 AND kind='declaration' AND redacted_at IS NULL AND ingested_at<=?2 AND json_valid(content) AND json_extract(content,'$.op')='register_predicate' ORDER BY ingested_at,seq").map_err(sql_err)?;
    let rows = declarations
        .query_map(params![space, known_at], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?;
    for r in rows {
        let (name, json) = r.map_err(sql_err)?;
        let definition = serde_json::from_str(&json)
            .map_err(|e| BrainError::Storage(format!("historical predicate: {e}")))?;
        definitions.insert(name, definition);
    }
    let mut groups = Vec::new();
    let mut live_sources = std::collections::BTreeSet::new();
    let mut entities = std::collections::BTreeSet::new();
    for group in selected {
        let entries = crate::knowledge::get_statement_group_at(
            conn,
            space,
            &group.subject,
            &group.predicate,
            Some(Timestamp(known_at)),
        )?;
        for entry in &entries {
            entities.insert(entry.statement.subject.clone());
            if let oxibrain_core::Object::Entity(id) = &entry.statement.object {
                entities.insert(id.clone());
            }
            live_sources.extend(entry.assertions.iter().map(|a| a.episode.clone()));
        }
        groups.push(entries);
    }
    let identities = crate::truth_groups::identities(conn, space, Timestamp(known_at), &entities)?;
    live_sources.extend(identities.merges.iter().map(|m| m.provenance.clone()));
    Ok(TruthInput {
        merge_surfaces: identities.merge_surfaces,
        merges: identities.merges,
        entity_types: identities.types,
        groups,
        definitions,
        live_sources,
        surfaces: identities.surfaces,
        omitted_groups: 0,
    })
}

#[derive(Debug, Clone)]
pub struct DerivedSource {
    pub id: String,
    pub text: String,
    pub sources: Vec<String>,
    pub uncertainty: Option<String>,
    pub revision: String,
    pub support_states: Vec<(bool, bool, bool)>,
}

pub fn summaries(
    conn: &Connection,
    space: &str,
    known: Timestamp,
    valid: Timestamp,
) -> Result<Vec<DerivedSource>, BrainError> {
    let known = known.0;
    let valid = valid.0;
    let mut q=conn.prepare("SELECT id,content,uncertainty_json,hex(content_hash) FROM episodes WHERE space_id=?1 AND kind='derived' AND redacted_at IS NULL AND ingested_at<=?2 AND occurred_at<=?3 ORDER BY seq DESC LIMIT 64").map_err(sql_err)?;
    let rows = q
        .query_map(params![space, known, valid], |r| {
            Ok(DerivedSource {
                id: r.get(0)?,
                text: r.get(1)?,
                uncertainty: r.get(2)?,
                revision: r.get(3)?,
                sources: Vec::new(),
                support_states: Vec::new(),
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for mut row in rows {
        let mut q=conn.prepare("SELECT l.to_episode,e.id,e.space_id,e.redacted_at,e.kind,e.ingested_at FROM episode_links l LEFT JOIN episodes e ON e.id=l.to_episode WHERE l.from_episode=?1 AND l.rel='summarizes' ORDER BY l.to_episode").map_err(sql_err)?;
        let supports = q
            .query_map([&row.id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })
            .map_err(sql_err)?;
        for support in supports {
            let (id, present, source_space, redacted, kind, ingested) = support.map_err(sql_err)?;
            row.sources.push(id);
            row.support_states.push((
                present.is_some() && redacted.is_none() && ingested.is_some_and(|t| t <= known),
                source_space.as_deref() == Some(space),
                kind.as_deref().is_some_and(|k| k != "derived"),
            ));
        }
        out.push(row);
    }
    Ok(out)
}

pub fn missing_source_count(conn: &Connection, space: &str) -> Result<usize, BrainError> {
    conn.query_row("SELECT count(*) FROM episodes e WHERE e.space_id=?1 AND e.kind='primary' AND (e.content!='' OR e.content_compacted IS NOT NULL) AND e.redacted_at IS NULL AND e.source_kind NOT IN ('document','document_revision') AND NOT EXISTS (SELECT 1 FROM memory_source_chunks c JOIN memory_chunk_fts_word f ON f.rowid=c.rowid WHERE c.episode_id=e.id)",[space],|r|r.get(0)).map_err(sql_err)
}

/// At most two session heads for continuity, fetched after query candidates.
pub fn recent_sessions(
    conn: &Connection,
    scope: &SourceReadScope<'_>,
) -> Result<Vec<RawSourceHit>, BrainError> {
    let mut q = conn.prepare("SELECT c.id,e.id,CAST(substr(CASE WHEN e.content='' THEN e.content_compacted ELSE CAST(e.content AS BLOB) END,c.span_start+1,c.span_end-c.span_start) AS TEXT),c.span_start,c.span_end,hex(e.content_hash),e.source_kind,e.source_ref,e.ingested_at,e.occurred_at,e.trust FROM episodes e JOIN memory_source_chunks c ON c.episode_id=e.id AND c.ordinal=0 WHERE e.space_id=?1 AND e.source_kind='session' AND e.kind='primary' AND e.redacted_at IS NULL AND e.ingested_at<=?2 AND e.occurred_at<=?3 AND (?4 IS NULL OR json_extract(e.source_ref,'$.session_id') IS NOT ?4) ORDER BY e.seq DESC LIMIT 2").map_err(sql_err)?;
    q.query_map(
        params![
            scope.space,
            scope.known_at.0,
            scope.as_of.0,
            scope.excluded_session
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
    )
    .map_err(sql_err)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(sql_err)
}

fn raw_index_available(conn: &Connection) -> Result<bool, BrainError> {
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_source_chunks')",[],|r|r.get(0)).map_err(sql_err)?;
    if !exists {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sql_err)?;
        if version >= 15 {
            return Err(BrainError::Corruption(
                "schema 15 is missing its raw-source index".into(),
            ));
        }
    }
    Ok(exists)
}
