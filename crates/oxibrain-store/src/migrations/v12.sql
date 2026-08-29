-- v12: entity embeddings stored as symmetric int8 BLOBs (ADR-014, §7.4).
-- sqlite-vec 0.1.x classifies every INSERT blob as float32, so vec0 int8
-- columns cannot take byte blobs; entity_vectors becomes a plain table and
-- KNN moves to Rust-side exact L2 (crate::vectors). Ranking-half state:
-- rebuildable by reproject(); the migration converts existing float rows in
-- place via the Rust step in migration.rs (scale-1.0 symmetric quantization,
-- clamped — encoder output is L2-normalized so |x_i| <= 1).

DROP TABLE IF EXISTS entity_vectors;
CREATE TABLE IF NOT EXISTS entity_vectors (
  entity_id TEXT PRIMARY KEY,
  embedding BLOB NOT NULL
);
