-- Rebuild disposable raw-source chunks, FTS entries, and embeddings under
-- source-paragraph-window/3 using the smaller generic source windows.
DELETE FROM memory_source_chunks;
DELETE FROM meta WHERE key LIKE 'raw-index/%';
