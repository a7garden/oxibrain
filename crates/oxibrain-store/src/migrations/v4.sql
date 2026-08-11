-- v4: tokens + redactions for M4 security.
-- Tokens: operational auth state (random, not content-derived, not reprojection).
CREATE TABLE IF NOT EXISTS tokens (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL UNIQUE,
    scope_json   TEXT NOT NULL,
    issued_at    INTEGER NOT NULL,
    issued_by    TEXT NOT NULL,
    revoked_at   INTEGER,
    label        TEXT
);
CREATE INDEX IF NOT EXISTS idx_tokens_hash ON tokens(token_hash);
CREATE INDEX IF NOT EXISTS idx_tokens_revoked ON tokens(revoked_at);

-- Redactions: durable record of what was redacted. Reprojection replays these
-- AFTER replaying extractions, so entity-scoped redaction survives a reproject.
-- Episode-scoped redaction is handled by `redacted_at IS NULL` filters on the
-- replay queries; these rows still exist for the audit trail and idempotency.
CREATE TABLE IF NOT EXISTS redactions (
    id          TEXT PRIMARY KEY,
    target_json TEXT NOT NULL,
    reason      TEXT NOT NULL,
    actor       TEXT NOT NULL,
    redacted_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_redactions_ts ON redactions(redacted_at);
