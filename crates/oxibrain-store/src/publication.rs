//! Mechanical fetches and writes for the episode-backed publication projection.
use crate::sql_err;
use oxibrain_core::publication::{
    MAX_CLOSURE_VIEWS, PublicationEventV1, PublicationRecord, ViewState, plan_projection,
};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, OptionalExtension, params};

pub fn records(conn: &Connection) -> Result<Vec<PublicationRecord>, BrainError> {
    let mut statement = conn
        .prepare("SELECT record_json FROM publication_views ORDER BY id LIMIT ?1")
        .map_err(sql_err)?;
    let raw = statement
        .query_map([MAX_CLOSURE_VIEWS as i64 + 1], |row| {
            row.get::<_, String>(0)
        })
        .map_err(sql_err)?;
    let mut total_bytes = 0usize;
    let mut records = Vec::new();
    for value in raw {
        let value = value.map_err(sql_err)?;
        total_bytes = total_bytes.saturating_add(value.len());
        if total_bytes > oxibrain_core::publication::MAX_PROJECTION_BYTES {
            return Err(BrainError::Budget(
                "publication projection materialization bound exceeded".into(),
            ));
        }
        records.push(
            serde_json::from_str(&value)
                .map_err(|_| BrainError::Corruption("invalid publication projection".into()))?,
        );
    }
    Ok(records)
}
pub fn record(conn: &Connection, id: &str) -> Result<Option<PublicationRecord>, BrainError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT record_json FROM publication_views WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    raw.map(|value| {
        serde_json::from_str(&value)
            .map_err(|_| BrainError::Corruption("invalid publication projection".into()))
    })
    .transpose()
}
pub fn audience_record(
    conn: &Connection,
    destination: &str,
    id: &str,
) -> Result<Option<PublicationRecord>, BrainError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT record_json FROM publication_views WHERE id=?1 AND destination=?2",
            params![id, destination],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    raw.map(|value| {
        serde_json::from_str(&value)
            .map_err(|_| BrainError::Corruption("invalid publication projection".into()))
    })
    .transpose()
}
pub fn write_record(conn: &Connection, record: &PublicationRecord) -> Result<(), BrainError> {
    let raw = serde_json::to_string(record)
        .map_err(|_| BrainError::Storage("publication serialization failed".into()))?;
    conn.execute("INSERT INTO publication_views(id,destination,principal,occurrence_id,record_json) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET record_json=excluded.record_json",
        params![record.id,record.destination,record.principal,record.occurrence_id,raw]).map_err(sql_err)?;
    Ok(())
}
pub fn project_event(
    conn: &Connection,
    declaration: &str,
    space: &str,
    event: &PublicationEventV1,
) -> Result<(), BrainError> {
    let rows = records(conn)?;
    let plan = plan_projection(event, declaration, space, &rows, &observations(conn)?)?;
    if let Some(record) = plan.insert {
        write_record(conn, &record)?;
    }
    for id in plan.close {
        if let Some(mut row) = record(conn, &id)? {
            row.state = ViewState::Closed;
            row.bytes.clear();
            write_record(conn, &row)?;
        }
    }
    if let PublicationEventV1::SourceObserved {
        origin_identity,
        previous_revision,
        observed_revision,
        ..
    } = event
    {
        conn.execute("INSERT OR IGNORE INTO publication_source_observations(declaration_id,origin_identity,previous_revision,observed_revision) VALUES (?1,?2,?3,?4)",
            params![declaration,origin_identity,previous_revision,observed_revision]).map_err(sql_err)?;
    }
    Ok(())
}

/// Fetch only the originating Declaration reference by following bounded parent
/// links. Internal source identities never enter audience-facing DTOs.
pub fn root_origin(
    conn: &Connection,
    row: &PublicationRecord,
) -> Result<oxibrain_core::publication::ViewOrigin, BrainError> {
    use oxibrain_core::publication::{MAX_LINEAGE_DEPTH, unavailable};
    let mut current = row.clone();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..MAX_LINEAGE_DEPTH {
        if !seen.insert(current.id.clone()) {
            return Err(unavailable());
        }
        if let Some(parent) = current.parent_id {
            current = record(conn, &parent)?.ok_or_else(unavailable)?;
            continue;
        }
        let episode =
            crate::ledger::get_episode(conn, &current.declaration_id)?.ok_or_else(unavailable)?;
        if episode.redacted_at.is_some() {
            return Err(unavailable());
        }
        let declaration =
            crate::project::parse_declaration(&episode.content).map_err(|_| unavailable())?;
        if let crate::project::Declaration::PublicationV1 {
            event: PublicationEventV1::Published { request, .. },
        } = declaration
        {
            return Ok(request.origin);
        }
        return Err(unavailable());
    }
    Err(unavailable())
}

pub fn redaction_declarations(
    conn: &Connection,
    episodes: &[String],
    spaces: &[String],
) -> Result<Vec<String>, BrainError> {
    oxibrain_core::publication::publication_redaction_declarations(
        &records(conn)?,
        episodes,
        spaces,
    )
}
pub fn remove_redacted_copies(
    conn: &Connection,
    declarations: &[String],
) -> Result<(), BrainError> {
    for row in records(conn)? {
        if declarations.contains(&row.declaration_id) {
            conn.execute("DELETE FROM publication_views WHERE id=?1", [&row.id])
                .map_err(sql_err)?;
        }
    }
    Ok(())
}

/// Immutable operation receipt lookup uses the existing Declaration ledger.
/// No independently durable receipt table is introduced.
pub fn operation_event(
    conn: &Connection,
    space: &str,
    principal: &str,
    occurrence: &str,
    kind: &str,
) -> Result<Option<PublicationEventV1>, BrainError> {
    let raw: Option<String> = conn.query_row(
        "SELECT content FROM episodes WHERE space_id=?1 AND kind='declaration' AND redacted_at IS NULL
         AND CASE WHEN json_valid(content) THEN json_extract(content,'$.op') END='publication_v1'
         AND CASE WHEN json_valid(content) THEN json_extract(content,'$.event.event') END=?4
         AND CASE WHEN json_valid(content) THEN json_extract(content,'$.event.principal') END=?2
         AND CASE WHEN json_valid(content) THEN json_extract(content,'$.event.occurrence_id') END=?3 ORDER BY seq LIMIT 1",
        params![space,principal,occurrence,kind], |row| row.get(0)).optional().map_err(sql_err)?;
    raw.map(|value| match crate::project::parse_declaration(&value)? {
        crate::project::Declaration::PublicationV1 { event } => Ok(event),
        _ => Err(BrainError::Corruption("invalid publication receipt".into())),
    })
    .transpose()
}

pub fn observations(
    conn: &Connection,
) -> Result<Vec<oxibrain_core::publication::SourceViewObservation>, BrainError> {
    let mut statement = conn.prepare("SELECT origin_identity,previous_revision,observed_revision FROM publication_source_observations ORDER BY declaration_id LIMIT ?1").map_err(sql_err)?;
    statement
        .query_map([MAX_CLOSURE_VIEWS as i64 + 1], |row| {
            Ok(oxibrain_core::publication::SourceViewObservation {
                origin_identity: row.get(0)?,
                previous_revision: row.get(1)?,
                observed_revision: row.get(2)?,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)
}

/// Bound a source before fetching its bytes, after the facade's scope gate.
pub fn source_episode(
    conn: &Connection,
    space: &str,
    id: &str,
) -> Result<Option<oxibrain_core::Episode>, BrainError> {
    let eligible: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM episodes WHERE id=?1 AND space_id=?2 AND kind='primary' AND redacted_at IS NULL AND max(length(CAST(content AS BLOB)),coalesce(length(content_compacted),0))<=?3)", params![id,space,oxibrain_core::publication::MAX_SOURCE_BYTES as i64], |row| row.get(0)).map_err(sql_err)?;
    if !eligible {
        return Ok(None);
    }
    crate::ledger::get_episode_in_space(conn, space, id)
}

/// Reserved Declarations use exact source-occurrence identity. Legacy
/// normalized-content deduplication cannot identify publication operations.
pub fn insert_declaration_event(
    conn: &Connection,
    episode: &mut oxibrain_core::Episode,
    event: &PublicationEventV1,
) -> Result<(), BrainError> {
    use crate::project::{Declaration, ResolutionCache, project_declaration};
    use oxibrain_core::publication::{PUBLICATION_VERSION, exact_digest};
    let source = match crate::ledger::get_source_by_name(conn, &episode.space, PUBLICATION_VERSION)?
    {
        Some(source) => source,
        None => {
            project_declaration(
                conn,
                &episode.space,
                &Declaration::RegisterSource {
                    name: PUBLICATION_VERSION.into(),
                    kind: "publication_control".into(),
                    mode: "push".into(),
                    claims_json: "{}".into(),
                },
                episode.ingested_at,
                &mut ResolutionCache::new(),
            )?;
            crate::ledger::get_source_by_name(conn, &episode.space, PUBLICATION_VERSION)?
                .ok_or_else(|| {
                    BrainError::Corruption("publication control source missing".into())
                })?
        }
    };
    let identity = match event {
        PublicationEventV1::Published { request, principal } => serde_json::json!([
            "publish",
            principal,
            request.destination,
            request.occurrence_id
        ]),
        PublicationEventV1::Withdrawn {
            occurrence_id,
            destination,
            principal,
            ..
        } => serde_json::json!(["withdraw", principal, destination, occurrence_id]),
        PublicationEventV1::SourceObserved {
            occurrence_id,
            origin_identity,
            principal,
            ..
        } => serde_json::json!(["source_observed", principal, origin_identity, occurrence_id]),
    };
    let principal = match event {
        PublicationEventV1::Published { principal, .. }
        | PublicationEventV1::Withdrawn { principal, .. }
        | PublicationEventV1::SourceObserved { principal, .. } => principal.clone(),
    };
    let attachment = crate::ledger::IngestAttachment {
        source_id: source.id,
        occurrence_id: exact_digest(&oxibrain_core::canonical_json_value(&identity)),
        accepted_at: episode.ingested_at,
        principal,
        claims_json: "{}".into(),
    };
    crate::ledger::insert_event(conn, episode, Some(&attachment))?;
    let stored = crate::ledger::get_episode(conn, &episode.id)?
        .ok_or_else(|| BrainError::Corruption("publication event missing".into()))?;
    if stored.kind != oxibrain_core::EpisodeKind::Declaration
        || stored.source != oxibrain_core::SourceRef::Declaration
        || stored.redacted_at.is_some()
        || stored.content != episode.content
    {
        return Err(BrainError::Conflict(
            "publication event conflicts with original occurrence".into(),
        ));
    }
    Ok(())
}
