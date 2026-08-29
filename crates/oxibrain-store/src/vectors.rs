//! Dense embedding vector storage.
//!
//! Wraps the `entity_vectors` plain table. Vectors are 1024-dim f32
//! (BGE-M3, the default multilingual embedder) stored as **symmetric int8
//! blobs** (ADR-014, §7.4 storage budget): quantized with scale 1.0 at the
//! store boundary — encoder output is L2-normalized so `|x_i| <= 1` — and
//! dequantized on read. Callers hand `&[f32]` in and get `Vec<f32>` out.
//!
//! sqlite-vec 0.1.x classifies every INSERT blob as float32, so vec0 int8
//! columns cannot take byte blobs; the table is plain and KNN is an exact
//! Rust-side L2 scan. The corpus per space is small (entity surface count)
//! and the scan is a few MB — well under the sqlite-vec round-trip it
//! replaces. Vectors are projection (derived) — `reproject()` rebuilds
//! them. See ARCHITECTURE.md §9.1.
//!
//! The sqlite-vec extension must be loaded via `migration::ensure_vec_extension()`
//! before opening any connection (other vec0 tables may exist).

use crate::sql_err;
use oxibrain_index::{dequantize_i8, quantize_i8_unit};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Default embedding dimension. Matches BGE-M3 (the shipped default embedder).
/// Migrated from 384 (all-MiniLM-L6-v2) at schema v7.
pub const EMBEDDING_DIM: usize = 1024;

/// Upsert a dense embedding vector for an entity. Overwrites any existing vector.
pub fn upsert_vector(
    conn: &Connection,
    entity_id: &str,
    embedding: &[f32],
) -> Result<(), BrainError> {
    assert_eq!(
        embedding.len(),
        EMBEDDING_DIM,
        "embedding dimension mismatch: expected {EMBEDDING_DIM}, got {}",
        embedding.len()
    );
    conn.execute(
        "INSERT OR REPLACE INTO entity_vectors(entity_id, embedding) VALUES (?1, ?2)",
        params![entity_id, quantize_i8_unit(embedding)],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Batch-fetch dense vectors for a set of entity IDs (§11.4, 10.3 — MMR).
/// Entities without a vector are silently skipped. Returns at most one
/// vector per ID.
pub fn fetch_vectors_for_entities(
    conn: &Connection,
    entity_ids: &[String],
) -> Result<std::collections::HashMap<String, Vec<f32>>, BrainError> {
    let mut result = std::collections::HashMap::new();
    for id in entity_ids {
        let row = conn.query_row(
            "SELECT embedding FROM entity_vectors WHERE entity_id = ?1",
            params![id],
            |r| {
                let blob: Vec<u8> = r.get(0)?;
                Ok(dequantize_i8(&blob))
            },
        );
        if let Ok(vec) = row {
            result.insert(id.clone(), vec);
        }
    }
    Ok(result)
}

/// Delete the embedding vector for an entity. No-op if not present.
pub fn delete_vector(conn: &Connection, entity_id: &str) -> Result<(), BrainError> {
    conn.execute(
        "DELETE FROM entity_vectors WHERE entity_id = ?1",
        params![entity_id],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// A semantic KNN hit from the entity_vectors table.
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub entity_id: String,
    /// Exact L2 distance (lower = closer).
    pub distance: f64,
}

/// Top-k semantic nearest neighbors for a query vector.
/// Exact Rust-side L2 over the stored int8 rows, sorted ascending.
pub fn knn_search(
    conn: &Connection,
    query: &[f32],
    k: usize,
) -> Result<Vec<VectorHit>, BrainError> {
    assert_eq!(
        query.len(),
        EMBEDDING_DIM,
        "query dimension mismatch: expected {EMBEDDING_DIM}, got {}",
        query.len()
    );
    let q = quantize_i8_unit(query);
    let mut stmt = conn
        .prepare("SELECT entity_id, embedding FROM entity_vectors")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(sql_err)?;
    let mut scored: Vec<VectorHit> = Vec::new();
    for row in rows {
        let (entity_id, blob) = row.map_err(sql_err)?;
        // Exact integer L2 (ADR-014): both sides are int8, so the distance
        // is computed on the integer domain — self-query is exactly 0.
        let stored: &[u8] = &blob;
        let dist: f64 = q
            .iter()
            .zip(stored)
            .map(|(&a, &b)| {
                let d = (a as i64) - (b as i8 as i64);
                (d * d) as f64
            })
            .sum::<f64>()
            .sqrt();
        scored.push(VectorHit {
            entity_id,
            distance: dist,
        });
    }
    scored.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    scored.truncate(k);
    Ok(scored)
}

/// Count of entities with embedding vectors. Useful for tests and diagnostics.
pub fn count_vectors(conn: &Connection) -> Result<i64, BrainError> {
    conn.query_row("SELECT COUNT(*) FROM entity_vectors", [], |r| r.get(0))
        .map_err(sql_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration;

    #[test]
    fn round_trip_vector_insert_and_knn() {
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        // Insert two vectors (clamped into [-1, 1] on write).
        let v1: Vec<f32> = (0..EMBEDDING_DIM)
            .map(|i| (i as f32 * 0.001).clamp(-1.0, 1.0))
            .collect();
        let v2: Vec<f32> = (0..EMBEDDING_DIM)
            .map(|i| ((i as f32 * 0.001) - 0.9).clamp(-1.0, 1.0))
            .collect();
        upsert_vector(&conn, "e1", &v1).unwrap();
        upsert_vector(&conn, "e2", &v2).unwrap();
        assert_eq!(count_vectors(&conn).unwrap(), 2);

        // Storage is int8: one byte per dimension.
        let len: i64 = conn
            .query_row(
                "SELECT LENGTH(embedding) FROM entity_vectors WHERE entity_id = 'e1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(len as usize, EMBEDDING_DIM, "int8 blob");

        // KNN search: v1 should be closest to itself.
        let hits = knn_search(&conn, &v1, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entity_id, "e1");
        assert!(
            hits[0].distance.abs() < 1e-9,
            "self-distance ~0 (int8 exact)"
        );
    }

    #[test]
    fn upsert_overwrites_existing() {
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        let v: Vec<f32> = vec![0.0; EMBEDDING_DIM];
        upsert_vector(&conn, "e1", &v).unwrap();
        let v2: Vec<f32> = vec![1.0; EMBEDDING_DIM];
        upsert_vector(&conn, "e1", &v2).unwrap();
        assert_eq!(count_vectors(&conn).unwrap(), 1);
    }

    #[test]
    fn delete_removes_vector() {
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        let v: Vec<f32> = vec![0.0; EMBEDDING_DIM];
        upsert_vector(&conn, "e1", &v).unwrap();
        assert_eq!(count_vectors(&conn).unwrap(), 1);
        delete_vector(&conn, "e1").unwrap();
        assert_eq!(count_vectors(&conn).unwrap(), 0);
    }

    #[test]
    fn v12_migration_converts_float_rows_in_place() {
        migration::ensure_vec_extension();
        // Migrate to v11 (vec0 FLOAT[1024] table), insert one float row,
        // then run() to v12 and prove the row survives as int8.
        let conn = Connection::open_in_memory().unwrap();
        for v in 1..=11 {
            let sql = std::fs::read_to_string(format!(
                "{}/src/migrations/v{v}.sql",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            if v == 5 || v == 7 {
                migration::ensure_vec_extension();
            }
            conn.execute_batch(&sql).unwrap();
            conn.pragma_update(None, "user_version", v).unwrap();
        }
        let floats: Vec<f32> = (0..EMBEDDING_DIM)
            .map(|i| (i as f32 * 0.0005).clamp(-1.0, 1.0))
            .collect();
        let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT INTO entity_vectors(entity_id, embedding) VALUES ('e1', ?1)",
            rusqlite::params![bytes],
        )
        .unwrap();

        migration::run(&conn).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'entity_vectors'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sql.contains("embedding BLOB NOT NULL"),
            "recreated as a plain int8 blob table, got: {sql}"
        );
        let (n, len): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), LENGTH(embedding) FROM entity_vectors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "the float row survives as a converted int8 row");
        assert_eq!(len as usize, EMBEDDING_DIM);
        // Round-trips through the quantized API.
        let got = fetch_vectors_for_entities(&conn, &["e1".to_string()]).unwrap();
        let v = &got["e1"];
        assert!(v[0] - floats[0] < 0.01, "dequantizes near the original");
    }
}
