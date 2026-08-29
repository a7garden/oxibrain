# ADR-014 — Storage footprint: contentless FTS, int8 vectors, junk-path elimination

Status: accepted (implemented in 0.11.0). Date: 2026-08-28.

## Context

Measured on a real instance (303 episodes / 249 KB content): the ranking
half multiplied content ~7×. Two full body copies lived in the FTS content
tables (fts_word + fts_ngram: 736 KB), dense vectors cost 4 KB/row (f32),
720+ of 1024 source-registry rows were orphans (a tempdir-path leak), and
extraction failures grew forever — quarantine was an archive, not a retry
queue. Separately, `rebuild_fts` read `episodes.content` directly, which is
`''` after compaction, so compacted episodes silently vanished from search.

## Decision

1. **Brain FTS becomes contentless** (`fts_word` / `fts_ngram` with
   `content=''`, `contentless_delete=1`) plus a small `fts_map` rowid
   table: zero body copies; `episodes.content` (or `content_compacted`)
   is the single text source. External-content FTS was rejected for the
   brain plane: there is no single content table — targets are episodes,
   statements, and entity surfaces in three different tables.
2. **Documents FTS becomes external-content** on a new `doc_texts` table
   (the doc plane HAS one canonical content table), plus plain int8
   `doc_vectors`: one body copy instead of two.
3. **All dense vectors store as symmetric int8** (quantize against
   scale 1.0 for L2-normalized embeddings, clamped; per-vector max-abs
   for tfidf): 4× smaller. Cosine and L2 ordering are invariant under the
   per-vector rescale, so no scale metadata is stored. sqlite-vec 0.1.x
   classifies every INSERT blob as float32 — vec0 int8 columns reject byte
   blobs — so `entity_vectors` (v12) and `doc_vectors` (documents v2)
   become plain BLOB tables and KNN moves to Rust-side **exact integer L2**
   (the corpora are small; the scan is a few MB).
4. **Junk paths eliminated**: v13 deletes orphan source rows (rows still
   referenced by episodes stay — provenance, P2); a successful extraction
   consumes its matching `extraction_failures` rows (quarantine is a retry
   queue); `rebuild_fts` reads the effective episode text so compaction no
   longer removes episodes from search; doctor reports the orphan count.
5. **Rejected: bit (1-bit) quantization** — sign vectors would need a
   f32 rescore path to keep ranking quality, which defeats the size win.

## Consequences

- Per-episode marginal cost drops from ≈ 5× + 4 KB to ≈ 5× − body copies ≈
  2–3× + 1 KB (measured: the proof instance shrank 7.7 MB → 6.6 MB, with
  the FTS content tables gone entirely and orphan sources cleaned).
- Truth-half reprojection is untouched (byte-identical determinism holds);
  the ranking half is equivalent, not identical (§5.1 tolerance).
- Upgrading drops `doc_vectors` rows once: re-embed via `index --embed`.
- `plan_stale`-style rails are unaffected; the migration chain v11 → v13
  runs inside the normal store-open path with up-tests per version.
