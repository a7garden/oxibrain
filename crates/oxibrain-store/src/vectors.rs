//! Dense embedding vector storage (sqlite-vec).
//!
//! Wraps the `entity_vectors` vec0 virtual table. Vectors are 1024-dim f32
//! (BGE-M3, the default multilingual embedder). Vectors are projection
//! (derived) — reproject() rebuilds them. See ARCHITECTURE.md §9.1.
//!
//! The sqlite-vec extension must be loaded via `migration::ensure_vec_extension()`
//! before opening any connection.

use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Default embedding dimension. Matches BGE-M3 (the shipped default embedder).
/// Migrated from 384 (all-MiniLM-L6-v2) at schema v7.
pub const EMBEDDING_DIM: usize = 1024;

/// Unsafe view: reinterpret an `f32` slice as bytes for sqlite-vec.
/// Caller MUST ensure the slice length matches `EMBEDDING_DIM`.
fn f32_slice_as_bytes(v: &[f32]) -> &[u8] {
    let len = std::mem::size_of_val(v);
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, len) }
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
    // vec0 doesn't support INSERT OR REPLACE — DELETE then INSERT.
    conn.execute(
        "DELETE FROM entity_vectors WHERE entity_id = ?1",
        params![entity_id],
    )
    .map_err(sql_err)?;
    conn.execute(
        "INSERT INTO entity_vectors(entity_id, embedding) VALUES (?1, ?2)",
        params![entity_id, f32_slice_as_bytes(embedding)],
    )
    .map_err(sql_err)?;
    Ok(())
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
    /// Distance metric from sqlite-vec (L2 by default; lower = closer).
    pub distance: f64,
}

/// Top-k semantic nearest neighbors for a query vector.
/// Returns hits sorted by distance ascending (closest first).
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
    let mut stmt = conn
        .prepare(
            "SELECT entity_id, distance
             FROM entity_vectors
             WHERE embedding MATCH ?1
             ORDER BY distance
             LIMIT ?2",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![f32_slice_as_bytes(query), k as i64], |r| {
            Ok(VectorHit {
                entity_id: r.get(0)?,
                distance: r.get(1)?,
            })
        })
        .map_err(sql_err)?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(sql_err)?);
    }
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
    use crate::migration;

    #[test]
    fn round_trip_vector_insert_and_knn() {
        migration::ensure_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        migration::run(&conn).unwrap();

        // Insert two vectors.
        let v1: Vec<f32> = (0..EMBEDDING_DIM).map(|i| i as f32 * 0.01).collect();
        let v2: Vec<f32> = (0..EMBEDDING_DIM).map(|i| i as f32 * 0.01 + 0.5).collect();
        upsert_vector(&conn, "e1", &v1).unwrap();
        upsert_vector(&conn, "e2", &v2).unwrap();
        assert_eq!(count_vectors(&conn).unwrap(), 2);

        // KNN search: v1 should be closest to itself.
        let hits = knn_search(&conn, &v1, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entity_id, "e1");
        assert_eq!(hits[0].distance, 0.0);
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
}
