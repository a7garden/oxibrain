//! Dense embedding vector storage (int8 in a plain table).
//!
//! Wraps the `entity_vectors` table. Vectors are 1024-dim, stored as
//! symmetric int8 with fixed scale 1 (`clamp(v, -1, 1) * 127`): one byte per
//! dimension, 4x smaller than f32. BGE-M3 (the default multilingual
//! embedder) L2-normalizes its output, so `|v_i| <= 1` holds and the clamp
//! is loss-free; out-of-range inputs clamp defensively. KNN is computed
//! Rust-side (sqlite-vec 0.1.x vec0 KNN is a full scan anyway, and its
//! int8 columns reject byte blobs). Vectors are projection (derived) --
//! reproject() rebuilds them. See ARCHITECTURE.md §9.1.
//!
//! The sqlite-vec extension is still loaded via
//! `migration::ensure_vec_extension()` for the documents-plane cache.

use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Default embedding dimension. Matches BGE-M3 (the shipped default embedder).
/// Migrated from 384 (all-MiniLM-L6-v2) at schema v7.
pub const EMBEDDING_DIM: usize = 1024;

/// Decode an int8 storage blob to f32.
fn decode_blob(blob: &[u8]) -> Vec<f32> {
    oxibrain_index::dequantize_i8(blob)
}

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
        params![entity_id, oxibrain_index::quantize_i8_fixed(embedding)],
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
                Ok(decode_blob(&blob))
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
    /// L2 distance on the dequantized scale (lower = closer).
    pub distance: f64,
}

/// Top-k semantic nearest neighbors for a query vector.
/// Returns hits sorted by distance ascending (closest first).
///
/// Rust-side full scan: the table holds one row per entity (thousands, not
/// millions), so a scan over 1 KB int8 rows is cache-friendly and matches
/// the sqlite-vec 0.1.x brute-force behavior it replaces.
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
    let q = oxibrain_index::quantize_i8_fixed(query);
    let mut stmt = conn
        .prepare("SELECT entity_id, embedding FROM entity_vectors")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            let id: String = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            Ok((id, blob))
        })
        .map_err(sql_err)?;
    let mut hits = Vec::new();
    for row in rows {
        let (id, blob) = row.map_err(sql_err)?;
        // Exact integer-domain L2, rescaled to the dequantized scale.
        let sum_sq: u64 = blob
            .iter()
            .zip(&q)
            .map(|(&a, &b)| {
                let d = i32::from(a as i8) - i32::from(b as i8);
                (d * d) as u64
            })
            .sum();
        hits.push(VectorHit {
            entity_id: id,
            distance: (sum_sq as f64).sqrt() / 127.0,
        });
    }
    hits.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .expect("finite distances")
    });
    hits.truncate(k);
    Ok(hits)
}

/// Count of entities with embedding vectors. Useful for tests and diagnostics.
pub fn count_vectors(conn: &Connection) -> Result<i64, BrainError> {
    conn.query_row("SELECT COUNT(*) FROM entity_vectors", [], |r| r.get(0))
        .map_err(sql_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        crate::migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        crate::migration::run(&conn).unwrap();
        conn
    }

    fn test_vec(seed: f32) -> Vec<f32> {
        (0..EMBEDDING_DIM)
            .map(|i| (seed * (i as f32 + 1.0) * 0.001) - 0.4)
            .collect::<Vec<f32>>()
            .into_iter()
            .map(|v| v.clamp(-1.0, 1.0))
            .collect()
    }

    #[test]
    fn upsert_fetch_roundtrip() {
        let conn = fresh();
        let v = test_vec(0.3);
        upsert_vector(&conn, "e1", &v).unwrap();
        let got = fetch_vectors_for_entities(&conn, &["e1".into()]).unwrap();
        let rt = got.get("e1").unwrap();
        // int8 quantization error bound: 0.5/127 per dimension.
        for (a, b) in v.iter().zip(rt) {
            assert!((a - b).abs() < 0.005, "per-dim drift {a} vs {b}");
        }
    }

    #[test]
    fn knn_orders_by_distance() {
        let conn = fresh();
        let base = test_vec(0.0);
        let near = test_vec(0.02);
        let far = test_vec(2.0);
        upsert_vector(&conn, "base", &base).unwrap();
        upsert_vector(&conn, "near", &near).unwrap();
        upsert_vector(&conn, "far", &far).unwrap();

        let hits = knn_search(&conn, &base, 3).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].entity_id, "base");
        assert_eq!(hits[1].entity_id, "near", "near beats far");
        assert_eq!(hits[2].entity_id, "far");
        assert!(hits[0].distance <= hits[1].distance && hits[1].distance <= hits[2].distance);
    }

    #[test]
    fn storage_is_one_byte_per_dim() {
        let conn = fresh();
        upsert_vector(&conn, "e1", &test_vec(0.5)).unwrap();
        let len: i64 = conn
            .query_row("SELECT LENGTH(embedding) FROM entity_vectors", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(len as usize, EMBEDDING_DIM);
    }

    #[test]
    fn delete_is_noop_when_absent() {
        let conn = fresh();
        delete_vector(&conn, "ghost").unwrap();
        assert_eq!(count_vectors(&conn).unwrap(), 0);
    }
}
