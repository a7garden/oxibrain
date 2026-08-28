-- v13: contentless FTS5 — zero body copies (storage-footprint plan, B1).
--
-- The FTS layer previously stored the full body twice (fts_word_content +
-- fts_ngram_content shadow tables) on top of episodes.content: ~3x the
-- text bytes. Both tables are now contentless (contentless_delete=1 since
-- SQLite 3.43; rusqlite 0.32 bundles 3.46) — they keep only their inverted
-- indexes. `fts_map` carries the rowid → target mapping the contentless
-- tables cannot store. Both are ranking-half state; the Rust migration
-- step repopulates them per space right after this file applies
-- (pure SQL + tokenization, no model calls).
--
-- Also: orphaned source-registry rows are deleted. A source row whose id
-- no episode references was registered by a scan/push that never landed an
-- episode (e.g. a tempdir root from a test run) and can never be referenced
-- again — the FK target would have to be an episode that does not exist.
-- Rows still referenced by episodes stay: provenance (P2).

DROP TABLE IF EXISTS fts_word;
DROP TABLE IF EXISTS fts_ngram;
CREATE VIRTUAL TABLE IF NOT EXISTS fts_word USING fts5(
  body,
  content='',
  contentless_delete=1
);
CREATE VIRTUAL TABLE IF NOT EXISTS fts_ngram USING fts5(
  body,
  content='',
  contentless_delete=1,
  tokenize = 'trigram'
);
CREATE TABLE IF NOT EXISTS fts_map (
  rowid       INTEGER PRIMARY KEY,
  space_id    TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_fts_map_target
  ON fts_map(space_id, target_kind, target_id);

DELETE FROM sources
 WHERE id NOT IN (SELECT source_id FROM episodes WHERE source_id IS NOT NULL);
