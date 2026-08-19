-- v10: source registry, trust policies, event identity, assertion trust.
--
-- Episodes table rebuild: drops UNIQUE(space_id, content_hash) which
-- conflated byte-equality with event identity (§4.2). Two independent
-- sources containing identical bytes are now two independent episodes.
-- The 12-step SQLite ALTER TABLE pattern is used because SQLite cannot
-- drop a UNIQUE constraint via ALTER TABLE.

PRAGMA foreign_keys=OFF;

-- Source registry (created before episodes_new so the FK resolves).
CREATE TABLE IF NOT EXISTS sources (
  id          TEXT PRIMARY KEY,
  space_id    TEXT NOT NULL REFERENCES spaces(id),
  name        TEXT NOT NULL,
  kind        TEXT NOT NULL,
  mode        TEXT NOT NULL CHECK (mode IN ('push', 'pull')),
  claims_json TEXT NOT NULL DEFAULT '{}',
  created_at  INTEGER NOT NULL,
  UNIQUE (space_id, name)
);

-- Trust policies.
CREATE TABLE IF NOT EXISTS source_policies (
  id             TEXT PRIMARY KEY,
  source_id      TEXT NOT NULL REFERENCES sources(id),
  trust          TEXT NOT NULL CHECK (trust IN ('trusted', 'semi_trusted', 'untrusted')),
  effective_from INTEGER NOT NULL,
  effective_to   INTEGER,
  declaration_ep TEXT NOT NULL REFERENCES episodes(id),
  created_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_source ON source_policies(source_id, effective_from);

-- Rebuild episodes without UNIQUE(space_id, content_hash).
CREATE TABLE episodes_new (
  id                TEXT PRIMARY KEY,
  space_id          TEXT NOT NULL REFERENCES spaces(id),
  seq               INTEGER NOT NULL,
  content_hash      BLOB NOT NULL,
  content           TEXT NOT NULL,
  source_kind       TEXT NOT NULL,
  source_ref        TEXT,
  trust             TEXT NOT NULL,
  kind              TEXT NOT NULL,
  occurred_at       INTEGER NOT NULL,
  ingested_at       INTEGER NOT NULL,
  redacted_at       INTEGER,
  content_compacted BLOB,
  compacted_at      INTEGER,
  uncertainty_json  TEXT,
  source_id         TEXT REFERENCES sources(id),
  occurrence_id     TEXT,
  accepted_at       INTEGER,
  principal         TEXT,
  claims_json       TEXT,
  UNIQUE (space_id, seq)
);

INSERT INTO episodes_new
  (id, space_id, seq, content_hash, content, source_kind, source_ref,
   trust, kind, occurred_at, ingested_at, redacted_at,
   content_compacted, compacted_at, uncertainty_json)
SELECT id, space_id, seq, content_hash, content, source_kind, source_ref,
       trust, kind, occurred_at, ingested_at, redacted_at,
       content_compacted, compacted_at, uncertainty_json
FROM episodes;

DROP TABLE episodes;

ALTER TABLE episodes_new RENAME TO episodes;

-- Partial unique index: event identity for new-path episodes only.
-- Legacy episodes (source_id IS NULL) are not constrained by this index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_ep_occurrence
  ON episodes(space_id, source_id, occurrence_id)
  WHERE source_id IS NOT NULL AND occurrence_id IS NOT NULL;

-- Assertion trust: which trust tier the supporting episode had at ingest.
ALTER TABLE assertions ADD COLUMN trust TEXT NOT NULL DEFAULT 'trusted';

PRAGMA foreign_keys=ON;