-- v14 — AgentTrace gains per-agent identity (experience memory spec §2.1).
-- New rows write {"agent": …} into episodes.source_ref; legacy unit-variant
-- rows have NULL. import-oxios is the only known legacy writer (verified
-- against the tree), so its rows backfill with that tag. No DDL: the ref
-- column already exists and is TEXT.
UPDATE episodes
   SET source_ref = '{"agent":"import-oxios"}'
 WHERE source_kind = 'agent_trace'
   AND (source_ref IS NULL OR source_ref = '');
