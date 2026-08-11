//! Ledger-zone writes/reads: spaces and episodes. M0 only; knowledge writes land in M1.

use crate::sql_err;
use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier, content_hash, episode_id};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

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
        return Ok(()); // no-op (DESIGN.md §7.3 idempotency layer 1)
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
        "declaration" => Ok(SourceRef::Declaration),
        "derived" => Ok(SourceRef::Derived {
            of: r#ref.unwrap_or_default(),
        }),
        other => Err(BrainError::Corruption(format!(
            "unknown source kind: {other}"
        ))),
    }
}
