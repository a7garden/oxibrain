-- FTS5 virtual table for lexical search over episode content + statement renderings.
CREATE VIRTUAL TABLE IF NOT EXISTS episodes_fts USING fts5(
    space_id UNINDEXED,
    target_kind,
    target_id,
    body,
    tokenize = 'porter unicode61'
);

-- TF-IDF vector storage (BLOB-serialized Vec<f32>).
CREATE TABLE IF NOT EXISTS tfidf_vectors (
    space_id    TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    vector      BLOB NOT NULL,
    PRIMARY KEY (space_id, target_kind, target_id)
);

-- Salience cache columns on entities.
ALTER TABLE entities ADD COLUMN salience REAL NOT NULL DEFAULT 1.0;
ALTER TABLE entities ADD COLUMN last_activity INTEGER;

-- Compaction columns on episodes.
ALTER TABLE episodes ADD COLUMN content_compacted BLOB;
ALTER TABLE episodes ADD COLUMN compacted_at INTEGER;
