//! Paged truth-group observations and scoped identity data. Selection is in core.
use crate::sql_err;
use oxibrain_core::truth_selection::GroupObservation;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

pub fn catalog(
    conn: &Connection,
    space: &str,
    known: Timestamp,
    after: Option<(&str, &str)>,
    limit: usize,
) -> Result<Vec<GroupObservation>, BrainError> {
    let (subject, predicate) = after.unwrap_or(("", ""));
    let mut q=conn.prepare("SELECT s.subject_id,s.predicate,sum(length(a.id)+length(a.episode_id)+length(s.id)+coalesce(length(CAST(s.object_literal AS BLOB)),0)+256) FROM statements s JOIN assertions a ON a.statement_id=s.id JOIN episodes e ON e.id=a.episode_id WHERE s.space_id=?1 AND a.recorded_at<=?2 AND e.ingested_at<=?2 AND e.redacted_at IS NULL AND (s.subject_id>?3 OR (s.subject_id=?3 AND s.predicate>?4)) GROUP BY s.subject_id,s.predicate ORDER BY s.subject_id,s.predicate LIMIT ?5").map_err(sql_err)?;
    let rows = q
        .query_map(params![space, known.0, subject, predicate, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, usize>(2)?,
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    let mut out = Vec::new();
    for (subject, predicate, bytes) in rows {
        // Search metadata is bounded independently of complete fold inputs.
        // Its loss is observable, never confused with group completeness.
        let mut q=conn.prepare("SELECT substr(text,1,512),length(text)>512 FROM (SELECT DISTINCT m.surface AS text FROM mentions m JOIN assertions a ON a.id=m.assertion_id JOIN statements s ON s.id=a.statement_id JOIN episodes e ON e.id=a.episode_id WHERE s.space_id=?1 AND s.subject_id=?2 AND s.predicate=?3 AND a.recorded_at<=?4 AND e.ingested_at<=?4 AND e.redacted_at IS NULL UNION SELECT DISTINCT s.object_literal AS text FROM statements s JOIN assertions a ON a.statement_id=s.id JOIN episodes e ON e.id=a.episode_id WHERE s.space_id=?1 AND s.subject_id=?2 AND s.predicate=?3 AND s.object_literal IS NOT NULL AND a.recorded_at<=?4 AND e.ingested_at<=?4 AND e.redacted_at IS NULL) ORDER BY text LIMIT 17").map_err(sql_err)?;
        let seed = q
            .query_map(params![space, subject, predicate, known.0], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        let seed_truncated = seed.len() > 16 || seed.iter().any(|(_, cut)| *cut);
        let search_text = std::iter::once(predicate.as_str())
            .chain(seed.iter().take(16).map(|(s, _)| s.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        out.push(GroupObservation {
            subject,
            predicate,
            bytes,
            search_text,
            seed_truncated,
        });
    }
    Ok(out)
}

pub struct IdentityInput {
    pub merges: Vec<oxibrain_core::EntityMerge>,
    pub merge_surfaces: std::collections::BTreeMap<String, String>,
    pub surfaces: std::collections::BTreeMap<String, String>,
    pub types: std::collections::BTreeMap<String, String>,
}
/// Fetch only the visible identity closure of the selected complete groups.
pub fn identities(
    conn: &Connection,
    space: &str,
    known: Timestamp,
    seeds: &std::collections::BTreeSet<String>,
) -> Result<IdentityInput, BrainError> {
    use oxibrain_core::{EntityMerge, MergeDecision};
    let json = serde_json::to_string(seeds).expect("identity ids serialize");
    let mut q=conn.prepare("WITH RECURSIVE ids(id) AS (SELECT value FROM json_each(?3) UNION SELECT CASE WHEN m.loser_id=i.id THEN m.winner_id ELSE m.loser_id END FROM ids i JOIN entity_merges m ON m.loser_id=i.id OR m.winner_id=i.id JOIN episodes e ON e.id=m.provenance WHERE e.space_id=?1 AND e.redacted_at IS NULL AND e.ingested_at<=?2 AND m.decided_at<=?2 AND (m.undone_at IS NULL OR m.undone_at>?2)) SELECT DISTINCT m.id,m.loser_id,m.winner_id,m.decided_by,m.score,m.provenance,m.decided_at,m.undone_at FROM entity_merges m JOIN episodes e ON e.id=m.provenance WHERE (m.loser_id IN (SELECT id FROM ids) OR m.winner_id IN (SELECT id FROM ids)) AND e.space_id=?1 AND e.redacted_at IS NULL AND e.ingested_at<=?2 AND m.decided_at<=?2 AND (m.undone_at IS NULL OR m.undone_at>?2) ORDER BY m.decided_at,m.id").map_err(sql_err)?;
    let merges = q
        .query_map(params![space, known.0, json], |r| {
            let decision: String = r.get(3)?;
            let score: Option<f64> = r.get(4)?;
            Ok(EntityMerge {
                id: r.get(0)?,
                loser: r.get(1)?,
                winner: r.get(2)?,
                decided_by: MergeDecision::parse_db(&decision, score)
                    .expect("valid merge decision"),
                provenance: r.get(5)?,
                evidence: Vec::new(),
                decided_at: Timestamp(r.get(6)?),
                undone_at: r.get::<_, Option<i64>>(7)?.map(Timestamp),
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    let mut ids = seeds.clone();
    let mut merge_surfaces = std::collections::BTreeMap::new();
    for merge in &merges {
        ids.insert(merge.loser.clone());
        ids.insert(merge.winner.clone());
        let surface:Option<String>=conn.query_row("SELECT CASE WHEN json_valid(content) THEN json_extract(content,'$.winner.surface') END FROM episodes WHERE id=?1",[&merge.provenance],|r|r.get(0)).map_err(sql_err)?;
        if let Some(surface) = surface {
            merge_surfaces.insert(merge.id.clone(), surface);
        }
    }
    let mut surfaces = std::collections::BTreeMap::new();
    let mut types = std::collections::BTreeMap::new();
    for id in ids {
        let mut q=conn.prepare("SELECT m.surface FROM mentions m JOIN assertions a ON a.id=m.assertion_id JOIN statements s ON s.id=a.statement_id JOIN episodes e ON e.id=a.episode_id WHERE s.space_id=?1 AND CASE m.role WHEN 'subject' THEN s.subject_id ELSE s.object_entity END=?2 AND a.recorded_at<=?3 AND e.ingested_at<=?3 AND e.redacted_at IS NULL ORDER BY a.recorded_at,m.id LIMIT 1").map_err(sql_err)?;
        let found = q
            .query_map(params![space, id, known.0], |r| r.get::<_, String>(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        if let Some(surface) = found.into_iter().next() {
            surfaces.insert(id.clone(), surface);
        }
        let mut q = conn
            .prepare(
                "SELECT type_name FROM entities WHERE space_id=?1 AND id=?2 AND created_at<=?3",
            )
            .map_err(sql_err)?;
        let found = q
            .query_map(params![space, id, known.0], |r| r.get::<_, String>(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        if let Some(ty) = found.into_iter().next() {
            types.insert(id, ty);
        }
    }
    Ok(IdentityInput {
        merges,
        merge_surfaces,
        surfaces,
        types,
    })
}
