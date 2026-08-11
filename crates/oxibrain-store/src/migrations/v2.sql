-- v2: fix mentions table FK bug from v1.
-- v1 had `id TEXT PRIMARY KEY REFERENCES assertions(id)` — wrong, MentionId ≠ AssertionId.
-- Drop and recreate without the spurious FK on `id`.
-- Safe: mentions table is empty at M0 exit (no knowledge writes yet).

DROP TABLE IF EXISTS mentions;
CREATE TABLE mentions (
  id           TEXT PRIMARY KEY,
  assertion_id TEXT NOT NULL REFERENCES assertions(id) ON DELETE CASCADE,
  role         TEXT NOT NULL,
  surface      TEXT NOT NULL,
  span_start   INTEGER NOT NULL,
  span_end     INTEGER NOT NULL,
  resolved_to  TEXT,
  method       TEXT NOT NULL
);
CREATE INDEX idx_mention_assert ON mentions(assertion_id);
