-- v2: single body copy + external-content FTS + plain int8 vectors.
--
-- doc_fts_word.body was the canonical text store (read back by
-- pending_vector_chunks), and doc_fts_ngram held a second full copy: two
-- copies of every chunk body. v2 keeps exactly one: `doc_texts`.
--
-- The FTS tables become external-content on doc_texts — they store only
-- their inverted indexes and read body/space/chunk_id values from the
-- content table on demand, so search SQL is unchanged. After the copy, the
-- 'rebuild' command re-indexes both from doc_texts.
--
-- doc_vectors drops vec0 for a plain int8 BLOB table: sqlite-vec 0.1.9
-- classifies every INSERT blob as float32 (int8/bit columns reject byte
-- blobs) and its KNN is a full scan anyway; Rust-side KNN over 1 KB int8
-- rows is identical in capability. Embeddings are recomputed on the next
-- `index --embed` (ranking-half cache, spec invariants §11).

CREATE TABLE IF NOT EXISTS doc_texts (
  rowid    INTEGER PRIMARY KEY,
  body     TEXT NOT NULL,
  space    TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE
);

-- Copy the one existing body copy (word index content) into doc_texts.
INSERT INTO doc_texts (rowid, body, space, chunk_id)
  SELECT rowid, body, space, chunk_id FROM doc_fts_word;

DROP TABLE doc_fts_word;
DROP TABLE doc_fts_ngram;
CREATE VIRTUAL TABLE doc_fts_word USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid'
);
CREATE VIRTUAL TABLE doc_fts_ngram USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid',
  tokenize = 'trigram'
);
INSERT INTO doc_fts_word(doc_fts_word) VALUES('rebuild');
INSERT INTO doc_fts_ngram(doc_fts_ngram) VALUES('rebuild');

DROP TABLE doc_vectors;
CREATE TABLE IF NOT EXISTS doc_vectors (
  chunk_id  TEXT PRIMARY KEY,
  embedding BLOB NOT NULL           -- int8[EMBEDDING_DIM], quantize_i8_fixed
);
