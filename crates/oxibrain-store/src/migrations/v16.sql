-- Disposable source ranking state. Rebuild from immutable source windows.
CREATE TABLE IF NOT EXISTS memory_source_vectors (
    chunk_id TEXT PRIMARY KEY REFERENCES memory_source_chunks(id) ON DELETE CASCADE,
    revision TEXT NOT NULL,
    window_policy TEXT NOT NULL,
    model_identity TEXT NOT NULL,
    dimensions INTEGER NOT NULL CHECK(dimensions > 0),
    embedding BLOB NOT NULL
);
CREATE TRIGGER IF NOT EXISTS memory_source_vector_delete AFTER DELETE ON memory_source_chunks BEGIN
    DELETE FROM memory_source_vectors WHERE chunk_id=OLD.id;
END;
