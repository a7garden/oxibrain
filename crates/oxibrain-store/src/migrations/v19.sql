-- Disposable projection of reserved PublicationV1 Declaration episodes.
-- Origin identities and family relations remain internal; no destination episode
-- or extracted assertion is produced by this projection.
CREATE TABLE IF NOT EXISTS publication_views (
    id TEXT PRIMARY KEY,
    destination TEXT NOT NULL,
    principal TEXT NOT NULL,
    occurrence_id TEXT NOT NULL,
    record_json TEXT NOT NULL,
    UNIQUE(destination, principal, occurrence_id)
);
CREATE INDEX IF NOT EXISTS publication_views_destination ON publication_views(destination, id);
CREATE TABLE IF NOT EXISTS publication_source_observations (
    declaration_id TEXT PRIMARY KEY,
    origin_identity TEXT NOT NULL,
    previous_revision TEXT NOT NULL,
    observed_revision TEXT
);
