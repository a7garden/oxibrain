-- Raw source windows have their own identity/coverage policy. Legacy contextual
-- `chunks` retain their non-overlapping spans and deterministic context prefixes.
CREATE TABLE IF NOT EXISTS memory_source_chunks (
  id TEXT PRIMARY KEY,
  space_id TEXT NOT NULL REFERENCES spaces(id),
  episode_id TEXT NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  span_start INTEGER NOT NULL,
  span_end INTEGER NOT NULL,
  UNIQUE(episode_id,ordinal)
);
CREATE INDEX IF NOT EXISTS memory_source_episode ON memory_source_chunks(episode_id);
-- Disposable raw-source indexes. Source bytes remain solely in the ledger.
CREATE VIRTUAL TABLE IF NOT EXISTS memory_chunk_fts_word USING fts5(body, content='', contentless_delete=1, tokenize='unicode61');
CREATE VIRTUAL TABLE IF NOT EXISTS memory_chunk_fts_ngram USING fts5(body, content='', contentless_delete=1, tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS memory_chunk_delete AFTER DELETE ON memory_source_chunks BEGIN
  DELETE FROM memory_chunk_fts_word WHERE rowid=OLD.rowid;
  DELETE FROM memory_chunk_fts_ngram WHERE rowid=OLD.rowid;
END;
CREATE TRIGGER IF NOT EXISTS memory_source_redact AFTER UPDATE OF redacted_at ON episodes
WHEN NEW.redacted_at IS NOT NULL BEGIN
  DELETE FROM memory_chunk_fts_word WHERE rowid IN (SELECT rowid FROM memory_source_chunks WHERE episode_id=NEW.id);
  DELETE FROM memory_chunk_fts_ngram WHERE rowid IN (SELECT rowid FROM memory_source_chunks WHERE episode_id=NEW.id);
  DELETE FROM memory_source_chunks WHERE episode_id=NEW.id;
END;

-- Operational exposure generation; never consulted by the truth fold.
CREATE TRIGGER IF NOT EXISTS exposure_insert AFTER INSERT ON episodes BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;
CREATE TRIGGER IF NOT EXISTS exposure_redact AFTER UPDATE OF redacted_at ON episodes BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;
CREATE TRIGGER IF NOT EXISTS exposure_delete AFTER DELETE ON episodes BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_assertions_insert AFTER INSERT ON assertions BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_assertions_update AFTER UPDATE ON assertions BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_assertions_delete AFTER DELETE ON assertions BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_entity_keys_insert AFTER INSERT ON entity_keys BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_entity_keys_update AFTER UPDATE ON entity_keys BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_entity_keys_delete AFTER DELETE ON entity_keys BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_entity_merges_insert AFTER INSERT ON entity_merges BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_entity_merges_update AFTER UPDATE ON entity_merges BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_entity_merges_delete AFTER DELETE ON entity_merges BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_predicates_insert AFTER INSERT ON predicates BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_predicates_update AFTER UPDATE ON predicates BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS exposure_predicates_delete AFTER DELETE ON predicates BEGIN
  INSERT INTO meta(key,value) VALUES('exposure_epoch','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;
END;

CREATE TRIGGER IF NOT EXISTS memory_source_delete AFTER DELETE ON episodes BEGIN
  DELETE FROM memory_source_chunks WHERE episode_id=OLD.id;
END;
