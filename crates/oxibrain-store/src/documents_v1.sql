-- v1: documents.db schema (Daemonless Two-Plane spec §6 verbatim, plus
-- `media_type` on `documents` and a `space` UNINDEXED column on both FTS
-- tables so lexical retrieval stays space-scoped).
--
-- This database is independent of brain.db. Its own PRAGMA user_version
-- starts at 1 and is owned by `crate::documents::DocumentCache::migrate`.

CREATE TABLE IF NOT EXISTS cache_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS doc_roots (
  alias          TEXT PRIMARY KEY,
  space          TEXT NOT NULL,
  config_hash    TEXT NOT NULL,
  generation     INTEGER NOT NULL,
  head_revision  TEXT,
  scanned_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS documents (
  id           TEXT PRIMARY KEY,
  root_alias   TEXT NOT NULL REFERENCES doc_roots(alias) ON DELETE CASCADE,
  space        TEXT NOT NULL,
  locator      TEXT NOT NULL,
  revision     TEXT NOT NULL,
  media_type   TEXT NOT NULL,
  bytes        INTEGER NOT NULL,
  modified_at  INTEGER NOT NULL,
  indexed_at   INTEGER NOT NULL,
  UNIQUE(root_alias, locator)
);

CREATE TABLE IF NOT EXISTS doc_manifest (
  root_alias   TEXT NOT NULL REFERENCES doc_roots(alias) ON DELETE CASCADE,
  locator      TEXT NOT NULL,
  bytes        INTEGER NOT NULL,
  modified_ns  INTEGER NOT NULL,
  revision     TEXT NOT NULL,
  PRIMARY KEY(root_alias, locator)
);

CREATE TABLE IF NOT EXISTS doc_chunks (
  id           TEXT PRIMARY KEY,
  document_id  TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  space        TEXT NOT NULL,
  ordinal      INTEGER NOT NULL,
  span_start   INTEGER NOT NULL,
  span_end     INTEGER NOT NULL,
  context      TEXT NOT NULL,
  UNIQUE(document_id, ordinal)
);

CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts_word USING fts5(
  body,
  space UNINDEXED,
  chunk_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts_ngram USING fts5(
  body,
  space UNINDEXED,
  chunk_id UNINDEXED,
  tokenize = 'trigram'
);

CREATE VIRTUAL TABLE IF NOT EXISTS doc_vectors USING vec0(
  chunk_id TEXT PRIMARY KEY,
  embedding FLOAT[1024]
);

CREATE INDEX IF NOT EXISTS idx_doc_chunks_space ON doc_chunks(space);
CREATE INDEX IF NOT EXISTS idx_documents_root   ON documents(root_alias);