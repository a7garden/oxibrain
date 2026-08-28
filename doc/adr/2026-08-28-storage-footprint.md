# ADR-014: Storage Footprint — Contentless FTS, int8 Vectors, Junk-Path Elimination

> **Status:** accepted (oxibrain v0.10, 2026-08-28). Replaces the storage-cost
> assumptions in §7.4 of `doc/ARCHITECTURE.md`.

## Context

The store grew faster than the data it indexes. Two structural wastes made
the multiplier worse than the design called for, and three leaky paths let
the database accumulate junk the design said wouldn't exist:

1. **FTS body copies.** `fts_word` and `fts_ngram` stored each chunk body
   twice in their FTS5 content shadow tables, on top of `episodes.content` —
   ~3× the text bytes. The brain and document planes both had this.
2. **Dense f32 vectors everywhere.** `entity_vectors`, `doc_vectors`, and
   `tfidf_vectors` stored 1024 × f32 = 4 KB per row, regardless of whether
   the column was float or, in the lexical channel, sparse.
3. **Leak paths.** The `sources` table accumulated 978 orphan rows on the
   reference store (paths from test tempdirs that never produced an episode).
   `extraction_failures` accumulated forever — only redaction deleted rows.
   `pre-v11.*.bak` files sat beside the database after every migration.
   `daemon.log` lived on after the daemon was retired.

## Decision

### 1. Brain FTS (v13): contentless FTS5 + `fts_map`

```sql
DROP TABLE fts_word; DROP TABLE fts_ngram;
CREATE VIRTUAL TABLE fts_word USING fts5(
  body, content='', contentless_delete=1
);
CREATE VIRTUAL TABLE fts_ngram USING fts5(
  body, content='', contentless_delete=1, tokenize='trigram'
);
CREATE TABLE fts_map (
  rowid INTEGER PRIMARY KEY,
  space_id TEXT NOT NULL, target_kind TEXT NOT NULL, target_id TEXT NOT NULL
);
```

`episodes.content` becomes the single text copy for episode + statement
targets; `entity_keys.surface` is the single copy for entity targets. `fts_map`
carries the rowid → target mapping the contentless tables cannot store.
Reads join `fts_map` on rowid; writes get the rowid via `last_insert_rowid`
after inserting the map row.

`contentless_delete=1` requires SQLite ≥ 3.43 (rusqlite 0.32 bundles 3.46).
Deletes are rowid-mediated: `SELECT rowid FROM fts_map WHERE ...` then
`DELETE FROM fts_word WHERE rowid IN (...)`.

### 2. Brain vectors (v12): entity embeddings int8, plain BLOB table

```sql
DROP TABLE entity_vectors;
CREATE TABLE entity_vectors (
  entity_id TEXT PRIMARY KEY,
  embedding BLOB NOT NULL      -- int8[1024], quantize_i8_fixed
);
```

**sqlite-vec 0.1.x cannot store int8 vec0 columns** — every INSERT blob is
classified as float32 ("expected to be of type int8, but a float32 vector
was provided"). Its vec0 KNN is a full scan anyway, so a plain BLOB table
with Rust-side exact integer L2 is algorithmically identical and stores
int8 (4× smaller). L2-normalized encoder output clamps losslessly at scale 1
(`v * 127`).

Rejected: bit quantization (32× smaller but requires a rescore path with
kept originals; we have no original f32 vectors to rescore against — the
table IS the cache).

### 3. TFIDF: symmetric int8, per-vector max-abs

```rust
pub fn quantize_i8(vec: &[f32]) -> Vec<u8> {
    let scale = max_abs(vec);
    vec.iter()
        .map(|&v| {
            let unit = if scale > 0.0 { (v / scale).clamp(-1.0, 1.0) } else { 0.0 };
            (unit * 127.0).round() as i8 as u8
        })
        .collect()
}
```

Cosine is invariant under positive per-vector rescaling: `cos(q, v)` on
int8 = `cos(q, v/scale)` on f32, so no scale metadata is stored. 4 KB → 1 KB
per row at dim 1024. No schema change (blob is blob); old rows are replaced
on the next `rebuild_tfidf`.

### 4. Document plane (documents.db v2): `doc_texts` + external-content FTS

```sql
CREATE TABLE doc_texts (rowid INTEGER PRIMARY KEY, body, space, chunk_id UNIQUE);
CREATE VIRTUAL TABLE doc_fts_word USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid'
);
-- + ngram variant
DROP TABLE doc_vectors; CREATE TABLE doc_vectors (chunk_id, embedding BLOB);  -- int8
```

External-content FTS5 keeps the search SQL unchanged (queries SELECT
`space`/`chunk_id` transparently from the content table). Inserts are
rowid-keyed; deletes are rowid-mediated — see §6 for the bug that drove this.

**Bug we hit and fixed.** External-content FTS5's `DELETE FROM fts_word
WHERE <non-rowid-col> IN (subquery)` — and even `WHERE chunk_id IN
(subquery)` — does **not** remove the postings from `fts_word_data`. The
content-table count drops but the index entries remain; a later `MATCH`
projection that needs the orphaned rowid's column values errors
"database disk image is malformed" because the content row is gone. Only
the rowid-mediated form removes postings correctly. All six delete sites
(purge, cascade_delete_root, delete_document, upsert pre-delete × word +
ngram) and the bulk space purge now resolve rowids from `doc_texts` first,
then delete FTS rows by rowid, then delete `doc_texts` rows by chunk_id.

### 5. Junk-path elimination

- **`extraction_failures` cleared on success.** A successful cache write
  now `DELETE FROM extraction_failures WHERE episode_id=?1 AND extractor_id=?2`.
  The quarantine is a retry queue, not an archive. Redaction remains the
  only other deleter.
- **`sources` orphan deletion at v13.** Migration runs
  `DELETE FROM sources WHERE id NOT IN (SELECT source_id FROM episodes WHERE source_id IS NOT NULL)`.
  Rows still referenced by episodes stay (provenance). `doctor` reports
  the orphan count since.
- **Legacy files deleted.** `brain.db.pre-v11.bak`,
  `brain.db.pre-v11.wal.bak`, `daemon.log` removed — all from prior to the
  daemonless two-plane migration.

### 6. Compacted-episode search bug

Pre-v13, `compact_episodes` cleared `episodes.content` and stored raw bytes
in `content_compacted`. `rebuild_fts` indexed `episodes.content` directly
(content='' after compaction) — compacted episodes silently left search.
Rebuild paths now use `effective_episode_content(content, content_compacted)`
(mirroring `ledger::get_episode`'s transparent decompression). This is in
the new `index_episode_fts` and `rebuild_chunks` plus `rebuild_fts`.

## Consequences

**Per-byte marginal cost of new episodes** (rough, dim 1024):

| | v11 | After | Factor |
|---|---|---|---|
| FTS body (episodes.content only) | 1× | 1× | 1× |
| FTS word+ngram inverted indexes | ≈0.7× + 3.4× | ≈0.7× + 3.4× | 1× (irreducible) |
| TFIDF dense vector (4 KB) | **4 KB** | **1 KB** | 4× |
| Entity vector (4 KB) | **4 KB** | **1 KB** | 4× |
| **Per-episode total** | **≈5× + 4 KB** | **≈5× + 1 KB** | |

**Reference store (303 episodes, before cleanup):**

- `fts_word_content` 368 KB → gone
- `fts_ngram_content` 368 KB → gone
- orphan sources 978 rows → 0
- `daemon.log` + `*.bak` 8.7 MB → removed

After this work, the only remaining growth paths are the FTS inverted
indexes (irreducible; the §7.4 chunk-level-only mitigation remains in
the toolbox) and the int8 vectors. §7.4's trigram-data > 3× word-data
threshold still holds (4.5× on this instance) — measured; the planned
mitigation is chunk-level-only indexing, not a representation change.

## Rejected alternatives

- **Compression in compaction.** `compact_episodes` still stores raw UTF-8
  bytes (no compression). Compression would reduce storage but doesn't fix
  the search bug and adds a dependency for negligible benefit on short
  episodic notes. Revisit when episodes carry large structured blobs.
- **External-content FTS5 for the brain plane.** Would have avoided the
  contentless+map machinery — but the brain has three target kinds
  (episodes, statements, entities) with no single content table. The map
  is the boring right answer.
- **Keep vec0 for `entity_vectors` / `doc_vectors`** via the alpha
  `sqlite-vec = "0.1.10-alpha.4"`. Risk for negligible benefit; the brute-
  force KNN is the same in both shapes.
- **Per-language detectors or stopword lists** to shrink trigram indexes.
  Forbidden by P11; the language-independent shape is structural, not a
  cost knob.
