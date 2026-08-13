-- v8: chunks table (DESIGN §5.7, M8 §8.11).
--
-- Chunks are slices of episode content, used for entity-dense retrieval and
-- contextual retrieval (§9.3). Chunk text is NOT stored — it is
-- substr(episodes.content, span_start, …). One copy of the bytes, and
-- redaction already tombstones the source.
--
-- The deterministic context prefix (§9.3) lives in `context` and is
-- generated from projection fields at projection time, not by a model
-- call. The owning row in `episodes` keeps the original bytes intact.
CREATE TABLE IF NOT EXISTS chunks (
  id         TEXT PRIMARY KEY,            -- blake3(episode_id, ordinal)
  space_id   TEXT NOT NULL REFERENCES spaces(id),
  episode_id TEXT NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  span_start INTEGER NOT NULL,            -- byte offsets into episodes.content
  span_end   INTEGER NOT NULL,
  context    TEXT NOT NULL,               -- deterministic prefix, §9.3
  UNIQUE (episode_id, ordinal)
);

CREATE INDEX IF NOT EXISTS idx_chunk_episode ON chunks(episode_id);
CREATE INDEX IF NOT EXISTS idx_chunk_space ON chunks(space_id);
