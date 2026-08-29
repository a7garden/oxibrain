-- v13: contentless FTS5 (ADR-014). The body is never stored in the FTS
-- layer; episodes.content (or content_compacted) is the single text copy.
-- fts_map carries the rowid -> target mapping the contentless tables
-- cannot. Both are ranking-half state, rebuilt by reproject.
--
-- The index repopulation runs in the Rust step of migration.rs (pure SQL +
-- tokenization; no model calls). Orphan source-registry rows — sources that
-- never produced an episode (the tempdir-path leak) — are deleted here;
-- rows still referenced by episodes stay (provenance, P2).

DROP TABLE IF EXISTS fts_word;
DROP TABLE IF EXISTS fts_ngram;
CREATE VIRTUAL TABLE IF NOT EXISTS fts_word USING fts5(
  body, content='', contentless_delete=1
);
CREATE VIRTUAL TABLE IF NOT EXISTS fts_ngram USING fts5(
  body, content='', contentless_delete=1
);
CREATE TABLE IF NOT EXISTS fts_map (
  rowid       INTEGER PRIMARY KEY,
  space_id    TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_fts_map_target
  ON fts_map(space_id, target_kind, target_id);

-- Off/on: chain fixtures may carry a spaces table without a PK, and the
-- FK parent-key check on sources would reject the DELETE even though the
-- deleted rows are referenced by nothing (v10 does the same around its
-- episodes rebuild).
PRAGMA foreign_keys=OFF;
DELETE FROM sources
 WHERE id NOT IN (SELECT source_id FROM episodes WHERE source_id IS NOT NULL);
PRAGMA foreign_keys=ON;
