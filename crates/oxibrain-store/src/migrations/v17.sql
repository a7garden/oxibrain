-- Raw source chunks and embeddings are disposable ranking state. Rebuild them
-- under source-paragraph-window/2 from immutable episode bytes.
DELETE FROM memory_source_chunks;
DELETE FROM meta WHERE key LIKE 'raw-index/%';
