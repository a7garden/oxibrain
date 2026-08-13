-- v7: entity vectors at 1024-dim (BGE-M3, the default embedder).
-- The v5 table was 384-dim (all-MiniLM-L6-v2), never populated in production
-- (F17). The default multilingual embedder is BGE-M3 (1024-dim), so the
-- table is recreated at that width. vec0 does not support ALTER of the
-- embedding dimension; drop + recreate is the migration path.
DROP TABLE IF EXISTS entity_vectors;
CREATE VIRTUAL TABLE entity_vectors USING vec0(
    entity_id TEXT PRIMARY KEY,
    embedding FLOAT[1024]
);
