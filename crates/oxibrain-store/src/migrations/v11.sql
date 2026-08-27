-- v11: queue-less extraction (two-plane design §11.1). The backlog is the
-- uncached_memory_episodes query; the durable queue is retired.

CREATE TABLE IF NOT EXISTS _v11_jobs AS SELECT COUNT(*) AS n FROM ingest_jobs;
INSERT OR REPLACE INTO meta (key, value)
  SELECT 'v11_ingest_jobs_dropped', CAST(n AS TEXT) FROM _v11_jobs;
DROP TABLE IF EXISTS _v11_jobs;
DROP TABLE IF EXISTS ingest_jobs;
