PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS spaces (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS episodes (
  id           TEXT PRIMARY KEY,
  space_id     TEXT NOT NULL REFERENCES spaces(id),
  seq          INTEGER NOT NULL,
  content_hash BLOB NOT NULL,
  content      TEXT NOT NULL,
  source_kind  TEXT NOT NULL,
  source_ref   TEXT,
  trust        TEXT NOT NULL,
  kind         TEXT NOT NULL,
  occurred_at  INTEGER NOT NULL,
  ingested_at  INTEGER NOT NULL,
  redacted_at  INTEGER,
  UNIQUE (space_id, content_hash),
  UNIQUE (space_id, seq)
);

CREATE TABLE IF NOT EXISTS episode_links (
  from_episode TEXT NOT NULL REFERENCES episodes(id),
  to_episode   TEXT NOT NULL REFERENCES episodes(id),
  rel          TEXT NOT NULL,
  PRIMARY KEY (from_episode, to_episode, rel)
);

CREATE TABLE IF NOT EXISTS extractions (
  episode_id    TEXT NOT NULL REFERENCES episodes(id),
  extractor_id  TEXT NOT NULL,
  response_hash BLOB NOT NULL,
  raw_response  TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (episode_id, extractor_id)
);

CREATE TABLE IF NOT EXISTS summaries (
  scope_kind      TEXT NOT NULL,
  member_set_hash BLOB NOT NULL,
  extractor_id    TEXT NOT NULL,
  text            TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (scope_kind, member_set_hash, extractor_id)
);

CREATE TABLE IF NOT EXISTS entities (
  id            TEXT PRIMARY KEY,
  space_id      TEXT NOT NULL REFERENCES spaces(id),
  type_name     TEXT NOT NULL,
  canonical_key TEXT REFERENCES entity_keys(id) DEFERRABLE INITIALLY DEFERRED,
  created_at    INTEGER NOT NULL,
  merged_into   TEXT REFERENCES entities(id)
);

CREATE TABLE IF NOT EXISTS entity_keys (
  id         TEXT PRIMARY KEY,
  space_id   TEXT NOT NULL REFERENCES spaces(id),
  entity_id  TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
  type_name  TEXT NOT NULL,
  normalized TEXT NOT NULL,
  surface    TEXT NOT NULL,
  origin     TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_entity_key_unique
  ON entity_keys(space_id, type_name, normalized);
CREATE INDEX IF NOT EXISTS idx_entity_key_entity ON entity_keys(entity_id);

CREATE TABLE IF NOT EXISTS entity_merges (
  id TEXT PRIMARY KEY,
  loser_id TEXT NOT NULL REFERENCES entities(id),
  winner_id TEXT NOT NULL REFERENCES entities(id),
  decided_by TEXT NOT NULL, score REAL,
  provenance TEXT REFERENCES episodes(id),
  decided_at INTEGER NOT NULL, undone_at INTEGER
);

CREATE TABLE IF NOT EXISTS statements (
  id             TEXT PRIMARY KEY,
  space_id       TEXT NOT NULL REFERENCES spaces(id),
  subject_id     TEXT NOT NULL REFERENCES entities(id),
  predicate      TEXT NOT NULL,
  object_entity  TEXT REFERENCES entities(id),
  object_literal TEXT,
  CHECK ((object_entity IS NULL) != (object_literal IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_stmt_subject ON statements(space_id, subject_id, predicate);
CREATE INDEX IF NOT EXISTS idx_stmt_object  ON statements(space_id, object_entity, predicate);

CREATE TABLE IF NOT EXISTS assertions (
  id           TEXT PRIMARY KEY,
  statement_id TEXT NOT NULL REFERENCES statements(id) ON DELETE CASCADE,
  episode_id   TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT,
  polarity     INTEGER NOT NULL,
  claimed_from INTEGER NOT NULL,
  claimed_to   INTEGER NOT NULL,
  confidence   REAL NOT NULL,
  recorded_at  INTEGER NOT NULL,
  retracted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_assert_stmt ON assertions(statement_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_assert_ep   ON assertions(episode_id);

CREATE TABLE IF NOT EXISTS beliefs (
  statement_id TEXT NOT NULL REFERENCES statements(id) ON DELETE CASCADE,
  valid_from   INTEGER NOT NULL,
  valid_to     INTEGER NOT NULL,
  status       TEXT NOT NULL,
  confidence   REAL NOT NULL,
  support_json TEXT NOT NULL,
  PRIMARY KEY (statement_id, valid_from)
);

CREATE TABLE IF NOT EXISTS mentions (
  id           TEXT PRIMARY KEY REFERENCES assertions(id),
  assertion_id TEXT NOT NULL REFERENCES assertions(id),
  role         TEXT NOT NULL,
  surface      TEXT NOT NULL,
  span_start   INTEGER NOT NULL,
  span_end     INTEGER NOT NULL,
  resolved_to  TEXT,
  method       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mention_assert ON mentions(assertion_id);

CREATE TABLE IF NOT EXISTS predicates (
  name         TEXT PRIMARY KEY,
  major_version INTEGER NOT NULL,
  minor_version INTEGER NOT NULL,
  def_json     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS communities (
  id        TEXT PRIMARY KEY,
  space_id  TEXT NOT NULL REFERENCES spaces(id),
  label     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_community_space ON communities(space_id);

CREATE TABLE IF NOT EXISTS ingest_jobs (
  id TEXT PRIMARY KEY, episode_id TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT NOT NULL, state TEXT NOT NULL,
  session_hint TEXT,
  attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
  lease_until INTEGER, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_ready ON ingest_jobs(state, lease_until);

CREATE TABLE IF NOT EXISTS extraction_failures (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  episode_id  TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT NOT NULL,
  raw_response TEXT NOT NULL,
  errors_json TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  ts        INTEGER NOT NULL,
  actor     TEXT NOT NULL,
  scope     TEXT,
  operation TEXT NOT NULL,
  target    TEXT,
  detail_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
