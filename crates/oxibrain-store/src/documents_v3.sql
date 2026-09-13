-- documents.db v3: PDC projection columns (doc/spec/pdc-adoption-v1.md).
-- A canonical PDC document records its envelope identity alongside the
-- existing projection row: the canonical UUID, the body profile
-- ('pdc-djot/1' | 'pdc-html/1'), the decoded envelope metadata as
-- serde_json of `oxibrain_core::documents::PdcProjectionMeta`, and the
-- trash flag (1 when the envelope says deleted:true, else 0/NULL).
--
-- Every column is nullable so v2 rows and legacy decoders keep working
-- with NULL. The partial unique index makes a UUID unique within one
-- root; the same document may legitimately live in several roots.

ALTER TABLE documents ADD COLUMN pdc_document_id TEXT;   -- canonical PDC UUID
ALTER TABLE documents ADD COLUMN pdc_body_profile TEXT;  -- 'pdc-djot/1' | 'pdc-html/1'
ALTER TABLE documents ADD COLUMN pdc_meta TEXT;          -- serde_json of PdcProjectionMeta
ALTER TABLE documents ADD COLUMN pdc_deleted INTEGER;    -- 1 when envelope deleted:true, else 0/NULL

CREATE UNIQUE INDEX IF NOT EXISTS idx_documents_pdc_uuid ON documents(root_alias, pdc_document_id)
  WHERE pdc_document_id IS NOT NULL;
