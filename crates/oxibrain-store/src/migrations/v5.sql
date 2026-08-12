-- v5: sqlite-vec entity embedding vectors + consolidation checkpoints.
-- Requires the sqlite-vec extension to be loaded (vec0 module).

-- Dense embedding vectors for semantic KNN search (§9.1).
-- Dimension is fixed at 384 (all-MiniLM-L6-v2 default).
-- The vec0 virtual table supports TEXT primary keys and FLOAT[] columns.
CREATE VIRTUAL TABLE IF NOT EXISTS entity_vectors USING vec0(
    entity_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);

-- Consolidation crash-recovery checkpoints (§10, sub-project L).
-- Cache table (not ledger) — reproject ignores and rebuilds.
CREATE TABLE IF NOT EXISTS consolidation_checkpoints (
    cluster_hash TEXT PRIMARY KEY,
    extractor_id TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    completed_at INTEGER
);
