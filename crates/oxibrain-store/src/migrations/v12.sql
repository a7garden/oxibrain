-- v12: entity embeddings stored int8 in a plain BLOB table.
--
-- sqlite-vec 0.1.x classifies every INSERT blob as float32 (int8/bit vec0
-- columns reject byte blobs: "expected to be of type int8, but a float32
-- vector was provided"), and its vec0 KNN is a full scan anyway -- no ANN
-- index ships in 0.1.x. A plain table with Rust-side KNN is algorithmically
-- identical, stores int8 (4x smaller than f32), and drops the vec0
-- constraint. entity_vectors is ranking-half derived state: the Rust
-- migration step reads the FLOAT[1024] rows BEFORE applying this file and
-- reinserts them quantized AFTER.

DROP TABLE IF EXISTS entity_vectors;
CREATE TABLE IF NOT EXISTS entity_vectors (
  entity_id TEXT PRIMARY KEY,
  embedding BLOB NOT NULL           -- int8[EMBEDDING_DIM], quantize_i8_fixed
);
