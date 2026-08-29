-- documents.db v2: single body copy + external-content FTS + int8 vectors
-- (ADR-014). doc_texts becomes the canonical decoded-text store (was: two
-- full copies inside the FTS content tables). The FTS tables become
-- external-content on doc_texts — search SQL is unchanged. doc_vectors
-- becomes a plain int8 blob table (sqlite-vec 0.1.x cannot take byte blobs
-- in vec0 int8 columns; KNN moves to Rust-side exact L2, mirroring brain.db
-- v12). Existing embeddings are dropped: re-embed with `index --embed`.

CREATE TABLE IF NOT EXISTS doc_texts (
  rowid INTEGER PRIMARY KEY,
  body  TEXT NOT NULL,
  space TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE
);

INSERT INTO doc_texts(rowid, body, space, chunk_id)
  SELECT rowid, body, space, chunk_id FROM doc_fts_word;

DROP TABLE IF EXISTS doc_fts_word;
CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts_word USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid'
);
INSERT INTO doc_fts_word(doc_fts_word) VALUES('rebuild');

DROP TABLE IF EXISTS doc_fts_ngram;
CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts_ngram USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid',
  tokenize = 'trigram'
);
INSERT INTO doc_fts_ngram(doc_fts_ngram) VALUES('rebuild');

DROP TABLE IF EXISTS doc_vectors;
CREATE TABLE IF NOT EXISTS doc_vectors (
  chunk_id TEXT PRIMARY KEY,
  embedding BLOB NOT NULL
);
