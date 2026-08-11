//! Reprojection: drop all projection tables and replay the ledger (DESIGN §14.3).
//! The single most valuable test in the suite — proves P1 (byte-identical rebuild).

use crate::project::{parse_declaration, project_declaration};
use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::Connection;

/// Drop all projection tables and replay Declaration episodes in canonical
/// (seq ASC) order. The result must be byte-identical to the incremental
/// projection (tested in the integration test suite).
pub fn reproject(conn: &Connection) -> Result<(), BrainError> {
    // 1. Delete all projection rows (order respects FK constraints).
    // Beliefs first (FK to statements), then mentions (FK to assertions),
    // then assertions (FK to statements), then statements,
    // then entity_merges, entity_keys, entities.
    for table in [
        "beliefs",
        "mentions",
        "assertions",
        "statements",
        "entity_merges",
        "entity_keys",
        "entities",
    ] {
        conn.execute(&format!("DELETE FROM {table}"), [])
            .map_err(sql_err)?;
    }

    // 2. Read all Declaration episodes in seq order, with their ingested_at.
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, content, ingested_at
             FROM episodes
             WHERE kind = 'declaration'
             ORDER BY seq ASC",
        )
        .map_err(sql_err)?;

    let episodes: Vec<(String, String, String, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,    // id
                r.get::<_, String>(1)?,    // space_id
                r.get::<_, String>(2)?,    // content
                r.get::<_, i64>(3)?,       // ingested_at
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;

    drop(stmt); // release the prepared statement before we write

    // 3. Replay each declaration, passing its ORIGINAL ingested_at as the
    //    transaction time. This reproduces the exact occurred_at/recorded_at/
    //    episode ids from the incremental path — required for byte-identical
    //    output. project_declaration is idempotent (INSERT OR IGNORE), so
    //    re-inserting rows is a no-op.
    for (_ep_id, space, content, ingested_at) in &episodes {
        let decl = parse_declaration(content)?;
        project_declaration(conn, space, &decl, Timestamp(*ingested_at))?;
    }
    drop(episodes);

    // 4. Rebuild indexes (FTS5, TF-IDF, salience) and communities for every
    //    space that survived replay. Index tables are derived state, so they
    //    must be rebuilt to match the freshly replayed projection — and doing
    //    so here makes reproject the canonical source of truth.
    let mut space_stmt = conn
        .prepare("SELECT DISTINCT id FROM spaces")
        .map_err(sql_err)?;
    let spaces: Vec<String> = space_stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(space_stmt);
    for space in &spaces {
        crate::index_ops::rebuild_indexes(conn, space)?;
        crate::communities::rebuild_communities(conn, space)?;
    }

    Ok(())
}
