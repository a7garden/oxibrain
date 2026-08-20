//! Ledger-zone writes/reads: spaces and episodes. M0 only; knowledge writes land in M1.

use crate::sql_err;
use oxibrain_core::{
    ContentHash, Episode, EpisodeKind, SourceRef, TrustTier, content_hash, episode_id,
    id::episode_event_id,
};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Idempotently create a space, returning its id. Id is derived from name (deterministic).
pub fn create_space(conn: &Connection, name: &str, now: Timestamp) -> Result<String, BrainError> {
    let id = space_id(name);
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM spaces WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    if n == 0 {
        conn.execute(
            "INSERT INTO spaces(id, name, created_at) VALUES(?1, ?2, ?3)",
            params![id, name, now.millis()],
        )
        .map_err(sql_err)?;
    }
    Ok(id)
}

/// Deterministic space id (blake3 of name). Keeps `init` reproducible.
fn space_id(name: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(name.as_bytes());
    let mut out = [0u8; 16];
    h.finalize_xof().fill(&mut out);
    hex::encode(out)
}

pub fn get_space(conn: &Connection, id: &str) -> Result<Option<String>, BrainError> {
    let name = match conn.query_row("SELECT name FROM spaces WHERE id = ?1", params![id], |r| {
        r.get(0)
    }) {
        Ok(n) => Some(n),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(sql_err(e)),
    };
    Ok(name)
}

/// Look up a space id by name (read-only). `Ok(None)` when the name has no
/// row. Use this in scope gates instead of `ensure_space` to avoid creating
/// rows on denied reads (P-scope: existence-enumeration side channel).
pub fn lookup_space_by_name(conn: &Connection, name: &str) -> Result<Option<String>, BrainError> {
    let id = match conn.query_row(
        "SELECT id FROM spaces WHERE name = ?1",
        params![name],
        |r| r.get(0),
    ) {
        Ok(id) => Some(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(sql_err(e)),
    };
    Ok(id)
}

/// A space row with live counts. Decision-free fetch (P9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceRow {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub episode_count: i64,
    pub entity_count: i64,
}

/// List all spaces with episode/entity counts, ordered by (created_at, id).
/// Canonical order — same store, same rows, same order.
pub fn list_spaces(conn: &Connection) -> Result<Vec<SpaceRow>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.name, s.created_at,
                (SELECT COUNT(*) FROM episodes e WHERE e.space_id = s.id),
                (SELECT COUNT(*) FROM entities en WHERE en.space_id = s.id)
             FROM spaces s
             ORDER BY s.created_at, s.id",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![], |r| {
            Ok(SpaceRow {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                episode_count: r.get(3)?,
                entity_count: r.get(4)?,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    Ok(rows)
}

/// Next monotonic seq for a space.
pub fn next_seq(conn: &Connection, space: &str) -> Result<u64, BrainError> {
    let max: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM episodes WHERE space_id = ?1",
            params![space],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    Ok(max.map(|m| m as u64 + 1).unwrap_or(0))
}

/// Insert an episode. Idempotent: re-inserting the same (space, content_hash) is a no-op.
/// Derives id, seq, content_hash. `episode.id`/`seq`/`content_hash` inputs are overwritten.
pub fn insert_episode(conn: &Connection, ep: &mut Episode) -> Result<(), BrainError> {
    let ch = content_hash(&ep.content);
    let id = episode_id(&ep.space, &ch, &ep.source, ep.occurred_at);
    // idempotency check
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE space_id = ?1 AND content_hash = ?2",
            params![ep.space, ch.as_bytes()],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    if exists > 0 {
        ep.id = id;
        ep.content_hash = ch;
        return Ok(()); // no-op (ARCHITECTURE.md §9.7 idempotency layer 1)
    }
    let seq = next_seq(conn, &ep.space)?;
    let (source_kind, source_ref) = ep.source.db_columns();
    conn.execute(
        "INSERT INTO episodes
         (id, space_id, seq, content_hash, content, source_kind, source_ref, trust, kind,
          occurred_at, ingested_at, redacted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            ep.space,
            seq,
            ch.as_bytes(),
            ep.content,
            source_kind,
            source_ref,
            ep.trust.as_db(),
            ep.kind.as_db(),
            ep.occurred_at.millis(),
            ep.ingested_at.millis(),
            ep.redacted_at.map(|t| t.millis()),
        ],
    )
    .map_err(sql_err)?;
    ep.id = id;
    ep.seq = seq;
    ep.content_hash = ch;
    Ok(())
}

/// Attachment metadata for the event-identity write path (§4.1).
/// All fields are server-assigned; never accepted from client payloads.
#[derive(Clone)]
pub struct IngestAttachment {
    pub source_id: String,
    pub occurrence_id: String,
    pub accepted_at: Timestamp,
    pub principal: String,
    pub claims_json: String,
}

/// Insert an episode via event identity (§4.2).
///
/// With `attachment`: identity is `(space_id, source_id, occurrence_id)`.
/// Without: delegates to `insert_episode` (legacy content-hash dedup).
///
/// Idempotent: re-inserting the same occurrence with identical bytes is a
/// no-op. Same occurrence with different bytes is `BrainError::Conflict`.
pub fn insert_event(
    conn: &Connection,
    ep: &mut Episode,
    attachment: Option<&IngestAttachment>,
) -> Result<(), BrainError> {
    let Some(att) = attachment else {
        return insert_episode(conn, ep);
    };

    let ch = content_hash(&ep.content);
    let id = episode_event_id(&ep.space, &att.source_id, &att.occurrence_id);

    // Check for existing episode with same event identity.
    let existing: Option<(String, i64, Vec<u8>)> = conn
        .query_row(
            "SELECT id, seq, content_hash FROM episodes
             WHERE space_id = ?1 AND source_id = ?2 AND occurrence_id = ?3",
            params![ep.space, att.source_id, att.occurrence_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql_err)?;

    match existing {
        Some((eid, seq, stored_hash)) => {
            if stored_hash == ch.as_bytes() {
                // Idempotent: same event identity, same bytes.
                ep.id = eid;
                ep.seq = seq as u64;
                ep.content_hash = ch;
                return Ok(());
            }
            Err(BrainError::Conflict(format!(
                "occurrence '{}' already exists with different content",
                att.occurrence_id
            )))
        }
        None => {
            let seq = next_seq(conn, &ep.space)?;
            let (source_kind, source_ref) = ep.source.db_columns();
            conn.execute(
                "INSERT INTO episodes
                 (id, space_id, seq, content_hash, content, source_kind, source_ref,
                  trust, kind, occurred_at, ingested_at, redacted_at,
                  source_id, occurrence_id, accepted_at, principal, claims_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16, ?17)",
                params![
                    id,
                    ep.space,
                    seq,
                    ch.as_bytes(),
                    ep.content,
                    source_kind,
                    source_ref,
                    ep.trust.as_db(),
                    ep.kind.as_db(),
                    ep.occurred_at.millis(),
                    ep.ingested_at.millis(),
                    ep.redacted_at.map(|t| t.millis()),
                    att.source_id,
                    att.occurrence_id,
                    att.accepted_at.millis(),
                    att.principal,
                    att.claims_json,
                ],
            )
            .map_err(sql_err)?;
            ep.id = id;
            ep.seq = seq;
            ep.content_hash = ch;
            Ok(())
        }
    }
}

pub fn get_episode(conn: &Connection, id: &str) -> Result<Option<Episode>, BrainError> {
    let row = conn.query_row(
        "SELECT id, space_id, seq, content_hash, content, source_kind, source_ref,
                 trust, kind, occurred_at, ingested_at, redacted_at, content_compacted
          FROM episodes WHERE id = ?1",
        params![id],
        |r| {
            let ch_blob: Vec<u8> = r.get(3)?;
            let mut ch = [0u8; 32];
            if ch_blob.len() == 32 {
                ch.copy_from_slice(&ch_blob);
            }
            let content: String = r.get(4)?;
            // Transparent decompression: if `content` is empty and
            // `content_compacted` is non-null, copy from the BLOB into
            // `content`. M2 stores raw bytes in the BLOB (no compression yet),
            // so this is a lossy UTF-8 conversion check: only valid UTF-8 is
            // restored; otherwise the caller sees the empty content.
            let content = if content.is_empty() {
                let compacted: Option<Vec<u8>> = r.get(12)?;
                match compacted {
                    Some(bytes) => String::from_utf8(bytes).unwrap_or_default(),
                    None => content,
                }
            } else {
                content
            };
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
                ch,
                content,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, Option<i64>>(11)?,
            ))
        },
    );
    match row {
        Ok((id, space, seq, ch, content, sk, sr, trust_s, kind_s, occ, ing, red)) => {
            let source = decode_source(&sk, sr)?;
            let trust = TrustTier::parse_db(&trust_s)
                .ok_or_else(|| BrainError::Corruption(format!("bad trust tier: {trust_s}")))?;
            let kind = EpisodeKind::parse_db(&kind_s)
                .ok_or_else(|| BrainError::Corruption(format!("bad episode kind: {kind_s}")))?;
            Ok(Some(Episode {
                id,
                space,
                seq,
                content_hash: oxibrain_core::ContentHash(ch),
                content,
                source,
                trust,
                kind,
                occurred_at: Timestamp(occ),
                ingested_at: Timestamp(ing),
                redacted_at: red.map(Timestamp),
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(sql_err(e)),
    }
}

/// Count episodes in the store (used by facade + tests so rusqlite stays in store).
pub fn episode_count(conn: &Connection) -> Result<i64, BrainError> {
    conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get::<_, i64>(0))
        .map_err(sql_err)
}

/// Note episodes' content hashes grouped by source path, for sync
/// classification. One query; decision-free (P9). Redacted episodes are
/// excluded — a redacted path has no live episode, so re-syncing its content
/// ingests afresh.
pub fn note_hashes_by_path(
    conn: &Connection,
    space: &str,
) -> Result<HashMap<String, HashSet<ContentHash>>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT source_ref, content_hash FROM episodes
             WHERE space_id = ?1 AND source_kind = 'note'
               AND source_ref IS NOT NULL AND redacted_at IS NULL",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(sql_err)?;
    let mut out: HashMap<String, HashSet<ContentHash>> = HashMap::new();
    for row in rows {
        let (path, hash) = row.map_err(sql_err)?;
        let mut bytes = [0u8; 32];
        if hash.len() == 32 {
            bytes.copy_from_slice(&hash);
        }
        out.entry(path).or_default().insert(ContentHash(bytes));
    }
    Ok(out)
}

/// Latest event-path episode state per locator for a source (§4.2 pull mode).
/// One query; decision-free (P9). Redacted episodes are excluded.
/// Returns the most recent (highest seq) episode per source_ref (locator).
pub fn locator_states(
    conn: &Connection,
    space: &str,
    source_id: &str,
) -> Result<HashMap<String, oxibrain_core::sync::LocatorState>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT source_ref, occurrence_id, content_hash FROM episodes
             WHERE space_id = ?1 AND source_id = ?2 AND redacted_at IS NULL
             ORDER BY seq ASC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sql_err)?;
    let mut out: HashMap<String, oxibrain_core::sync::LocatorState> = HashMap::new();
    for row in rows {
        let (locator, occ, hash) = row.map_err(sql_err)?;
        let mut bytes = [0u8; 32];
        if hash.len() == 32 {
            bytes.copy_from_slice(&hash);
        }
        // ORDER BY seq ASC → last write wins = latest episode.
        out.insert(
            locator,
            oxibrain_core::sync::LocatorState {
                latest_occurrence_id: occ,
                latest_content_hash: ContentHash(bytes),
            },
        );
    }
    Ok(out)
}

fn decode_source(kind: &str, r#ref: Option<String>) -> Result<SourceRef, BrainError> {
    match kind {
        "note" => Ok(SourceRef::Note {
            path: r#ref.unwrap_or_default(),
        }),
        "document" => Ok(SourceRef::Document {
            uri: r#ref.unwrap_or_default(),
        }),
        "conversation" => Ok(SourceRef::Conversation),
        "message" => Ok(SourceRef::Message),
        "agent_trace" => Ok(SourceRef::AgentTrace),
        "document_revision" => Ok(SourceRef::DocumentRevision {
            uri: r#ref.unwrap_or_default(),
        }),
        "artifact_event" => Ok(SourceRef::ArtifactEvent {
            uri: r#ref.unwrap_or_default(),
        }),
        "web_clip" => Ok(SourceRef::WebClip {
            uri: r#ref.unwrap_or_default(),
        }),
        "calendar_event" => Ok(SourceRef::CalendarEvent {
            uri: r#ref.unwrap_or_default(),
        }),
        "declaration" => Ok(SourceRef::Declaration),
        "derived" => Ok(SourceRef::Derived {
            of: r#ref.unwrap_or_default(),
        }),
        other => Err(BrainError::Corruption(format!(
            "unknown source kind: {other}"
        ))),
    }
}

// ── Source registry CRUD ────────────────────────────────────────────────────

/// A registered source row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRow {
    pub id: String,
    pub space: String,
    pub name: String,
    pub kind: String,
    pub mode: String,
    pub claims_json: String,
    pub created_at: Timestamp,
}

/// Insert a source (idempotent: same id → no-op).
pub fn insert_source(conn: &Connection, row: &SourceRow) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO sources (id, space_id, name, kind, mode, claims_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.id,
            row.space,
            row.name,
            row.kind,
            row.mode,
            row.claims_json,
            row.created_at.millis(),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Look up a source by (space, name).
pub fn get_source_by_name(
    conn: &Connection,
    space: &str,
    name: &str,
) -> Result<Option<SourceRow>, BrainError> {
    conn.query_row(
        "SELECT id, space_id, name, kind, mode, claims_json, created_at
         FROM sources WHERE space_id = ?1 AND name = ?2",
        params![space, name],
        |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                space: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                mode: r.get(4)?,
                claims_json: r.get(5)?,
                created_at: Timestamp(r.get::<_, i64>(6)?),
            })
        },
    )
    .optional()
    .map_err(sql_err)
}

/// List all sources in a space.
pub fn list_sources(conn: &Connection, space: &str) -> Result<Vec<SourceRow>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, name, kind, mode, claims_json, created_at
             FROM sources WHERE space_id = ?1 ORDER BY name",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                space: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                mode: r.get(4)?,
                claims_json: r.get(5)?,
                created_at: Timestamp(r.get::<_, i64>(6)?),
            })
        })
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

// ── Trust policy CRUD ───────────────────────────────────────────────────────

/// A source trust policy row.
pub struct PolicyRow {
    pub id: String,
    pub source_id: String,
    pub trust: TrustTier,
    pub effective_from: Timestamp,
    pub effective_to: Option<Timestamp>,
    pub declaration_ep: String,
    pub created_at: Timestamp,
}

/// Insert a policy (idempotent: same id → no-op).
pub fn insert_policy(conn: &Connection, row: &PolicyRow) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO source_policies
         (id, source_id, trust, effective_from, effective_to, declaration_ep, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.id,
            row.source_id,
            row.trust.as_db(),
            row.effective_from.millis(),
            row.effective_to.map(|t| t.millis()),
            row.declaration_ep,
            row.created_at.millis(),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Effective trust for a source at a given time: the latest policy whose
/// interval contains `at`. Returns None if no policy covers the instant.
pub fn effective_policy_trust(
    conn: &Connection,
    source_id: &str,
    at: Timestamp,
) -> Result<Option<TrustTier>, BrainError> {
    let trust_s: Option<String> = conn
        .query_row(
            "SELECT trust FROM source_policies
             WHERE source_id = ?1
               AND effective_from <= ?2
               AND (effective_to IS NULL OR effective_to > ?2)
             ORDER BY effective_from DESC
             LIMIT 1",
            params![source_id, at.millis()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    Ok(trust_s.and_then(|s| TrustTier::parse_db(&s)))
}
