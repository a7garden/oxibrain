# Storage Footprint Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate junk-accumulation paths (orphan sources, permanent extraction failures, legacy files) and cut the per-byte index multiplier from ~7× to ~2–3× by removing duplicate FTS body copies and quantizing dense vectors to int8.

**Architecture:** The ledger (P1) stays untouched. All changes hit the ranking half (FTS, tfidf, vectors — rebuildable derived state) and two write-path leaks. Brain-plane FTS becomes contentless FTS5 with a `fts_map` rowid table (zero body copies; `episodes.content` stays the single source). Document-plane FTS becomes external-content on a new `doc_texts` table (one body copy instead of two). All dense vectors become symmetric int8 (4× smaller; cosine is scale-invariant so no metadata column is needed).

**Tech Stack:** Rust 2024, rusqlite 0.32 (bundled SQLite 3.46 — supports `contentless_delete=1` since 3.43), sqlite-vec 0.1.9 (supports `int8[N]` vec0 columns since 0.1.6).

## Global Constraints

- P1/P5 invariants hold: ledger rows are never removed except by redaction; every changed table is ranking-half derived state, rebuildable by `reproject()` / `index --documents`.
- Schema changes ship with a migration + an up-test from the previous version fixture (`AGENTS.md` boundary).
- `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` clean; `cargo test` green after every task.
- `cargo build -p oxibrain --no-default-features --features http-llm` still passes (standalone guarantee).
- No new external dependencies. Quantization helpers live in `oxibrain-index`.
- Do NOT touch `crates/oxibrain-cli/src/cli.rs`, `crates/oxibrain-cli/src/main.rs`, `crates/oxibrain-cli/Cargo.toml`, `crates/oxibrain-cli/src/op.rs`, or `Cargo.lock` beyond what a task explicitly requires — they carry in-flight ADR-012 work.
- Comments and commit messages in English. Conventional commits (`feat:`, `fix:`, `perf:`, `test:`, `docs:`).

## Measured baseline (2026-08-28, `~/.oxi/brain`, 303 episodes / 249 KB content)

| Item | Size | Note |
|---|---|---|
| fts_ngram_data | 836 KB | trigram inverted index (3.4× content) — stays |
| fts_word_content + fts_ngram_content | 736 KB | duplicate body copies — **removed** by Task 4 |
| tfidf_vectors | 4 KB/row | dense f32 → **1 KB** by Task 2 |
| entity_vectors, doc_vectors | 4 KB/row | `FLOAT[1024]` → **1 KB** by Tasks 3/5 |
| orphan source rows | 720+ of 1024 | **deleted** by Task 4 migration |
| extraction_failures | grows forever | **cleared on success** by Task 1 |
| daemon.log + `*.bak` | 8.7 MB junk | **deleted** by Task 8 |

---

### Task 1: Clear extraction_failures when an extraction succeeds (A2)

**Files:**
- Modify: `crates/oxibrain-store/src/extraction.rs` (the function containing `INSERT OR REPLACE INTO extractions` at ~line 38)
- Test: `crates/oxibrain-store/tests/` — add `quarantine_clear.rs`

**Interfaces:**
- Produces: behavior change only. `oxibrain_store::quarantine::record_failure` unchanged; the extraction-cache write now also deletes that `(episode_id, extractor_id)`'s failure rows in the same transaction.

- [ ] **Step 1: Write the failing test**

```rust
// crates/oxibrain-store/tests/quarantine_clear.rs
//! A successful extraction consumes the episode's failure rows: quarantine
//! is a retry queue, not an archive (storage-footprint plan Task 1).

use oxibrain_ports::Timestamp;
use oxibrain_store::{extraction, quarantine, migration};
use rusqlite::Connection;

fn fresh() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    conn
}

fn episode(conn: &Connection) -> String {
    let mut ep = oxibrain_core::Episode::note("t.md", "hello world");
    ep.space = "sp".into();
    extraction::ingest_event(conn, &ep.space, &ep.content, ep.source.clone(), ep.trust, None, Timestamp(1000)).unwrap();
    let id: String = conn.query_row(
        "SELECT id FROM episodes WHERE space_id = 'sp'", [], |r| r.get(0)).unwrap();
    // ensure the space row exists for FK if needed; ingest_event handles it.
    id
}

#[test]
fn success_clears_prior_failures() {
    let conn = fresh();
    let ep = episode(&conn);
    quarantine::record_failure(&conn, &ep, "ext1", "garbage", r#"["bad json"]"#, Timestamp(2000)).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM extraction_failures", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);

    // The successful cache write must consume the failure rows.
    extraction::cache_extraction(&conn, &ep, "ext1", "{\"claims\":[]}", Timestamp(3000)).unwrap();

    let n: i64 = conn.query_row("SELECT COUNT(*) FROM extraction_failures", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "successful extraction must clear its failure rows");
}

#[test]
fn success_keeps_failures_of_other_extractors() {
    let conn = fresh();
    let ep = episode(&conn);
    quarantine::record_failure(&conn, &ep, "ext1", "garbage", "[]", Timestamp(2000)).unwrap();
    extraction::cache_extraction(&conn, &ep, "ext2", "{\"claims\":[]}", Timestamp(3000)).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM extraction_failures", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "only the succeeding extractor's rows are cleared");
}
```

Adapt the real function signatures from `extraction.rs`/`quarantine.rs` (the test compiles against them; check `record_failure`'s actual parameters — if it takes `episode_id: &str` instead of `&Episode`, bind accordingly).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p oxibrain-store --test quarantine_clear`
Expected: FAIL — `success_clears_prior_failures` sees `n == 1` after `cache_extraction`.

- [ ] **Step 3: Implement**

In the `extraction.rs` function that writes the extraction cache (the one with `INSERT OR REPLACE INTO extractions`), append in the same call:

```rust
// A successful extraction consumes the quarantine rows for this
// (episode, extractor) pair: failures are a retry queue, not an archive.
// Redaction is the only other deleter (§15.5).
conn.execute(
    "DELETE FROM extraction_failures WHERE episode_id = ?1 AND extractor_id = ?2",
    rusqlite::params![episode_id, extractor_id],
)
.map_err(sql_err)?;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p oxibrain-store --test quarantine_clear`
Expected: PASS (both tests).

- [ ] **Step 5: Run the crate suite + fmt/clippy, then commit**

```bash
cargo test -p oxibrain-store && cargo clippy -p oxibrain-store --all-targets -- -D warnings
git add crates/oxibrain-store/src/extraction.rs crates/oxibrain-store/tests/quarantine_clear.rs
git commit -m "fix: clear extraction_failures when the same extractor succeeds

Quarantine rows were only ever deleted by redaction, so every failed
extraction lived forever. A successful cache write now consumes the
matching (episode_id, extractor_id) rows; other extractors' rows stay."
```

---

### Task 2: tfidf int8 storage — 4 KB/row → 1 KB/row (B3)

Cosine is invariant under positive per-vector scaling, so storing `round(v_i / max|v| * 127)` as bytes preserves cosine ordering without any scale metadata.

**Files:**
- Modify: `crates/oxibrain-index/src/quantize.rs` (add int8 helpers; keep the existing sign-bit API untouched)
- Modify: `crates/oxibrain-index/src/lib.rs` (re-export)
- Modify: `crates/oxibrain-store/src/index_ops.rs` — `rebuild_tfidf` (line ~232)
- Modify: `crates/oxibrain-store/src/query.rs` — `load_knn_index` (line ~542)
- Test: `crates/oxibrain-index/src/quantize.rs` (unit tests inline), `crates/oxibrain-store/tests/` (roundtrip)

**Interfaces:**
- Produces (in `oxibrain_index::quantize`):
  - `pub fn max_abs(vec: &[f32]) -> f32` — `0.0` for empty input.
  - `pub fn quantize_i8(vec: &[f32]) -> Vec<u8>` — length `vec.len()`, each byte `round(clamp(v/max_abs, -1, 1) * 127)`; all-zero input → all-zero bytes (max_abs 0 → treat scale as 1).
  - `pub fn dequantize_i8(bytes: &[u8]) -> Vec<f32>` — `b as f32 / 127.0`.
- `tfidf_vectors.vector` keeps its BLOB column — no migration. Old f32 blobs are cleared by the rebuild itself (`DELETE FROM tfidf_vectors WHERE space_id = ?1` already in `rebuild_tfidf`), so schema version does not move in this task.

- [ ] **Step 1: Write the failing unit tests** (inline in `quantize.rs` `mod tests`)

```rust
#[test]
fn i8_roundtrip_preserves_cosine_ordering() {
    let a = vec![0.1, -0.4, 0.9, 0.0, 0.3, -0.2, 0.05, 0.7];
    let b = vec![0.12, -0.38, 0.85, 0.01, 0.28, -0.22, 0.04, 0.72];
    let c = vec![-0.9, 0.4, -0.1, 0.5, -0.3, 0.8, -0.05, -0.7];
    let cos = |x: &[f32], y: &[f32]| {
        let d: f32 = x.iter().zip(y).map(|(p, q)| p * q).sum();
        let na: f32 = x.iter().map(|p| p * p).sum::<f32>().sqrt();
        let nb: f32 = y.iter().map(|p| p * p).sum::<f32>().sqrt();
        d / (na * nb)
    };
    let qa = dequantize_i8(&quantize_i8(&a));
    let qb = dequantize_i8(&quantize_i8(&b));
    let qc = dequantize_i8(&quantize_i8(&c));
    assert!(cos(&qa, &qb) > cos(&qa, &qc), "similarity ordering must survive");
    assert!((cos(&qa, &qb) - cos(&a, &b)).abs() < 0.02, "cosine drift < 0.02");
}

#[test]
fn i8_zero_vector_is_all_zeros() {
    assert!(quantize_i8(&[0.0; 8]).iter().all(|&b| b == 0));
    assert_eq!(dequantize_i8(&[0; 8]), vec![0.0; 8]);
}

#[test]
fn i8_length_matches_input() {
    let v = vec![0.5f32; 100];
    assert_eq!(quantize_i8(&v).len(), 100);
}
```

- [ ] **Step 2: Run** `cargo test -p oxibrain-index quantize` — expect compile FAIL (functions missing).

- [ ] **Step 3: Implement helpers in `quantize.rs`**

```rust
/// Largest absolute component; 0.0 for an empty slice.
pub fn max_abs(vec: &[f32]) -> f32 {
    vec.iter().fold(0.0f32, |m, &v| m.max(v.abs()))
}

/// Symmetric int8 quantization against the vector's own max-abs component.
/// Cosine similarity is invariant under the per-vector rescale, so no scale
/// needs to be stored. An all-zero vector quantizes to all-zero bytes.
pub fn quantize_i8(vec: &[f32]) -> Vec<u8> {
    let scale = max_abs(vec);
    vec.iter()
        .map(|&v| {
            let unit = if scale > 0.0 { (v / scale).clamp(-1.0, 1.0) } else { 0.0 };
            (unit * 127.0).round() as i8 as u8
        })
        .collect()
}

/// Inverse of [`quantize_i8`] up to quantization error.
pub fn dequantize_i8(bytes: &[u8]) -> Vec<f32> {
    bytes.iter().map(|&b| b as i8 as f32 / 127.0).collect()
}
```

Re-export in `lib.rs` next to the existing `quantize` re-export.

- [ ] **Step 4: Switch the store paths**

`index_ops.rs::rebuild_tfidf` — replace `vector.to_bytes()` with `oxibrain_index::quantize_i8(model.transform(text).as_slice())`. Update the doc comment: "Vectors are stored symmetric-int8 (§7.4 storage budget): 1 KB/row, cosine-preserving."

`query.rs::load_knn_index` — where the blob is parsed with `TfIdfVector::from_bytes`, parse with `dequantize_i8` and re-wrap `TfIdfVector::from_vec(...)` instead. Check how `KnnIndex` consumes the vector and adapt (the cosine math itself is unchanged).

- [ ] **Step 5: Roundtrip test at the store level** — in `crates/oxibrain-store/tests/` (e.g. extend an existing tfidf/index test file): build a store, insert 2 episodes, run `rebuild_tfidf`, assert `LENGTH(vector) == dim` (1024 bytes) and `load_knn_index` returns hits for a query matching one episode's distinct n-grams.

- [ ] **Step 6: Run suites, commit**

```bash
cargo test -p oxibrain-index -p oxibrain-store && cargo clippy -p oxibrain-index -p oxibrain-store --all-targets -- -D warnings
git add crates/oxibrain-index/src/quantize.rs crates/oxibrain-index/src/lib.rs crates/oxibrain-store/src/index_ops.rs crates/oxibrain-store/src/query.rs
git commit -m "perf: store tfidf vectors as symmetric int8 (4KB -> 1KB per row)

Cosine is invariant under per-vector rescaling, so quantizing against
each vector's max-abs component preserves retrieval ordering with no
scale metadata. Old f32 rows are replaced by the next rebuild."
```

---

### Task 3: entity_vectors → `int8[1024]` (B2, brain plane)

**Files:**
- Create: `crates/oxibrain-store/src/migrations/v12.sql`
- Modify: `crates/oxibrain-store/src/migrations/mod.rs` (v12 step + up-test)
- Modify: `crates/oxibrain-store/src/schema.rs` — `LEDGER_SCHEMA_VERSION = 12`
- Modify: `crates/oxibrain-store/src/vectors.rs` — upsert/read quantize at the boundary
- Test: migration up-test in `mod.rs`; vector roundtrip in `vectors.rs` tests

**Interfaces:**
- Produces: `entity_vectors` is `vec0(entity_id TEXT PRIMARY KEY, embedding int8[1024])`. Rust callers still hand `&[f32]` to `upsert_*` and get `Vec<f32>` from reads — quantization is internal to `vectors.rs`. The vec0 KNN `MATCH` query binds an **int8 blob** (quantized with scale 1.0 — BGE-class embeddings are L2-normalized so `|x_i| <= 1`).

- [ ] **Step 1: Write the v12 up-test (failing first)** — in `migrations/mod.rs` tests, following the existing v11 pattern:

```rust
#[test]
fn up_v12_entity_vectors_int8() {
    let conn = Connection::open_in_memory().unwrap();
    // Run to v11 (existing helper or sequential run through v11.sql), then
    // insert one float row to prove the Rust step converts it.
    // ... seed v11 fixture, insert a FLOAT[1024] row via raw SQL ...
    let emb: Vec<u8> = vec![0u8; 4096]; // f32 bytes, all zeros
    conn.execute("INSERT INTO entity_vectors(entity_id, embedding) VALUES ('e1', ?1)",
        rusqlite::params![emb]).unwrap();

    migration::run(&conn).unwrap(); // brings to v12

    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE name = 'entity_vectors'", [],
        |r| r.get(0)).unwrap();
    assert!(sql.contains("int8[1024]"), "recreated as int8, got: {sql}");
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM entity_vectors", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "the float row survives as a converted int8 row");
}
```

- [ ] **Step 2: Run** `cargo test -p oxibrain-store up_v12` — FAIL (no v12).

- [ ] **Step 3: Write `v12.sql` + Rust step**

```sql
-- v12: entity embeddings stored int8 (storage footprint plan, B2).
-- entity_vectors is ranking-half derived state: drop + recreate + convert
-- in the Rust step. No truth-half table changes.

DROP TABLE IF EXISTS entity_vectors;
CREATE VIRTUAL TABLE IF NOT EXISTS entity_vectors USING vec0(
  entity_id TEXT PRIMARY KEY,
  embedding int8[1024]
);
```

Rust step in `mod.rs` before applying v12.sql (order: read old rows, apply sql, write converted rows): read `(entity_id, embedding blob)` from the old `FLOAT[1024]` table (blob = 4096 f32-LE bytes → `Vec<f32>` via `f32::from_le_bytes`), quantize with scale 1.0 — `v.clamp(-1.0,1.0) * 127.0 → i8` — and insert into the new table. Follow the existing migration-step pattern (look at how v2's `seed_core_v1` mixes SQL and Rust).

- [ ] **Step 4: `vectors.rs` boundary quantization**

- Upsert path (`upsert_entity_embeddings` / `embed_entities`): convert `&[f32]` → int8 blob before INSERT.
- Read path (the `SELECT embedding ... WHERE entity_id`): int8 blob → `Vec<f32>` (`b as i8 as f32 / 127.0`).
- KNN path (`embedding MATCH ?1`): quantize the query the same way; distance ordering is preserved (L2 on unit-scale int8).
- Update the module doc comment to state the representation and the assumption `|x_i| <= 1` (L2-normalized encoder output), and clamp defensively on write.

- [ ] **Step 5: Tests pass + suite**

```bash
cargo test -p oxibrain-store && cargo clippy -p oxibrain-store --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/oxibrain-store/src/migrations crates/oxibrain-store/src/schema.rs crates/oxibrain-store/src/vectors.rs
git commit -m "perf: entity embeddings int8 (v12) — 4KB -> 1KB per entity

vec0 int8 column with scale-1.0 symmetric quantization at the store
boundary; L2-normalized encoder output clamps losslessly. Migration
converts existing float rows in place."
```

---

### Task 4: brain FTS contentless + fts_map + orphan sources (B1 brain, A1) + compacted-episode read fix

The biggest one. `episodes.content` becomes the **only** body copy; both FTS tables keep just their inverted indexes. Also fixes: `rebuild_fts`/`rebuild_chunks` currently read `episodes.content` directly, which is `''` after `compact_episodes` moved the text into `content_compacted` — compacted episodes silently vanish from search. And the v13 Rust step deletes orphan `sources` rows (registered but never bound to an episode — the tempdir leak).

**Files:**
- Create: `crates/oxibrain-store/src/migrations/v13.sql`
- Modify: `crates/oxibrain-store/src/migrations/mod.rs` (v13 step + up-tests)
- Modify: `crates/oxibrain-store/src/schema.rs` — `LEDGER_SCHEMA_VERSION = 13`
- Modify: `crates/oxibrain-store/src/index_ops.rs` — `rebuild_fts`, `index_episode_fts`, `index_entities_fts`, `rebuild_chunks`, `snapshot_ranking`
- Modify: `crates/oxibrain-store/src/query.rs` — `fts_search` (join `fts_map`)
- Modify: `crates/oxibrain-store/src/redaction.rs` — FTS delete paths go through `fts_map` rowids
- Modify: `crates/oxibrain-cli/src/cmd/doctor.rs` — report orphan source count
- Test: up-tests in `mod.rs`; determinism via existing `m2_index_determinism.rs` (must still pass); new tests below

**Interfaces:**
- New table (v13.sql):

```sql
-- v13: contentless FTS5. The body is never stored in the FTS layer;
-- episodes.content (or content_compacted) is the single text copy.
-- fts_map carries the rowid → target mapping the contentless tables
-- cannot. Both are ranking-half state, rebuilt by reproject.

DROP TABLE IF EXISTS fts_word;
DROP TABLE IF EXISTS fts_ngram;
CREATE VIRTUAL TABLE IF NOT EXISTS fts_word USING fts5(
  body, content='', contentless_delete=1
);
CREATE VIRTUAL TABLE IF NOT EXISTS fts_ngram USING fts5(
  body, content='', contentless_delete=1
);
CREATE TABLE IF NOT EXISTS fts_map (
  rowid       INTEGER PRIMARY KEY,
  space_id    TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_fts_map_target
  ON fts_map(space_id, target_kind, target_id);

-- Orphaned source registry rows: sources that never produced an episode.
-- Rows still referenced by episodes stay (provenance, P2).
DELETE FROM sources
 WHERE id NOT IN (SELECT source_id FROM episodes WHERE source_id IS NOT NULL);
```

- Produces (helpers in `index_ops.rs`, used by every writer):

```rust
/// Allocate the next fts_map rowid for a space (monotonic per store).
fn next_map_rowid(conn: &Connection) -> Result<i64, BrainError>;

/// Insert one target into the map + both contentless indexes.
/// `content` must be the EFFECTIVE episode text (content, or the
/// content_compacted payload when content is empty).
fn fts_insert(conn: &Connection, space: &str, kind: &str, target_id: &str,
              content: &str) -> Result<(), BrainError>;

/// Remove one target's rows from the map + both indexes.
fn fts_delete_target(conn: &Connection, space: &str, kind: &str,
                     target_id: &str) -> Result<(), BrainError>;

/// Effective episode text: content, or the compacted payload when the
/// in-line column was cleared by compact_episodes.
fn effective_episode_content<'a>(content: &'a str, compacted: &'a [u8]) -> std::borrow::Cow<'a, str>;
```

`fts_delete_target` shape: `SELECT rowid FROM fts_map WHERE space_id=? AND target_kind=? AND target_id=?` → `DELETE FROM fts_word WHERE rowid IN (…)` / same for `fts_ngram` (legal with `contentless_delete=1`) → `DELETE FROM fts_map WHERE …`.

- [ ] **Step 1: Write failing up-test** — v11 fixture (with rows in old `fts_word`, one orphan source, one referenced source) → run migrations → assert: `fts_word`/`fts_ngram` SQL contains `contentless_delete=1`; `fts_map` exists; orphan source gone, referenced source kept; the Rust step repopulated the new indexes (row counts in `fts_map` match non-declaration, non-redacted episodes + statements + entity surfaces).

- [ ] **Step 2: Write failing behavior tests** (new file `crates/oxibrain-store/tests/fts_contentless.rs`):

```rust
//! Contentless FTS: zero body copies, compacted episodes stay searchable,
//! search results identical to the pre-v13 contract.

#[test]
fn fts_store_keeps_no_body_copy() {
    // build store, ingest "rareterm marker one", run rebuild_indexes
    // assert: SELECT COUNT(*) FROM fts_word == 1 row (map row exists)
    // and LENGTH(sqlite_master.sql) — the table no longer has a body
    // column: query "SELECT space_id FROM fts_word" must ERROR
    // (no such column) — proving contentless.
}

#[test]
fn compacted_episode_remains_searchable() {
    // ingest "compactme marker two"; compact_episodes(...90-day...);
    // rebuild_indexes; fts_search must return that episode id.
    // (Pre-v13 this failed: content='' was indexed.)
}

#[test]
fn search_finds_targets_after_reproject() {
    // ingest 3 episodes + 1 declare (statement target); reproject;
    // fts_search both indexes return episode + statement targets.
}
```

- [ ] **Step 3: Run** `cargo test -p oxibrain-store --test fts_contentless` — FAIL.

- [ ] **Step 4: Implement the migration** — `v13.sql` above; Rust step after DDL: rebuild both indexes by walking the same three passes `rebuild_fts` does (episodes with `effective_episode_content`, statements via `render_statement`, entity surfaces) — i.e. call the new `rebuild_fts` from the migration's Rust step (pure SQL + tokenization; no model calls; measured ~43 ms on the real store). Bump `LEDGER_SCHEMA_VERSION` to 13.

- [ ] **Step 5: Rewrite the writers** in `index_ops.rs`:
  - `rebuild_fts`: `DELETE FROM fts_map WHERE space_id = ?1` + delete both indexes' rows for those rowids (or plain `DELETE FROM fts_word WHERE rowid IN (SELECT rowid FROM fts_map WHERE space_id = ?1)` before clearing the map), then the three passes via `fts_insert`. Episodes pass reads `content, content_compacted` and uses `effective_episode_content`.
  - `index_episode_fts` / `index_entities_fts`: delegate to `fts_insert` / `fts_delete_target`; the incremental entity refresh stays idempotent (delete-then-insert).
  - `rebuild_chunks`: same `effective_episode_content` fix for span computation.
  - `snapshot_ranking`: the fts sections become `SELECT m.target_kind, m.target_id FROM fts_word f JOIN fts_map m ON m.rowid = f.rowid WHERE m.space_id = ?1 ORDER BY 1, 2` (membership, no body). Keep the section labels (`---fts_word---`) — `m2_index_determinism.rs` asserts them.

- [ ] **Step 6: Rewrite the reader** in `query.rs::fts_search`:

```sql
SELECT m.target_kind, m.target_id, f.rank
FROM fts_word f            -- or fts_ngram
JOIN fts_map m ON m.rowid = f.rowid
WHERE fts_word MATCH ?1 AND m.space_id = ?2
  AND NOT (m.target_kind = 'episode' AND EXISTS (
      SELECT 1 FROM episodes e
      WHERE e.id = m.target_id
        AND e.source_kind IN ('document', 'document_revision')))
ORDER BY f.rank
LIMIT ?3
```

(`MATCH` still names the fts table; the map join supplies the columns.)

- [ ] **Step 7: Redaction path** — in `redaction.rs`, replace `DELETE FROM fts_word WHERE space_id = ?1` / `fts_ngram` with the rowid-mediated delete (select map rowids for the space, delete from both indexes, clear map rows). The tfidf/chunks deletes there are unchanged.

- [ ] **Step 8: Doctor report** — in `doctor.rs`'s legacy section, add one line: orphaned source count (`sources` rows with no episode), labeled `orphan sources (never produced an episode): N`.

- [ ] **Step 9: Full determinism suite**

```bash
cargo test -p oxibrain-store && cargo test -p oxibrain --test m2_index_determinism
```

Expected: `reproject_determinism` still byte-identical (truth half untouched); ranking snapshot sections still present and equivalent.

- [ ] **Step 10: Commit**

```bash
git add crates/oxibrain-store crates/oxibrain-cli/src/cmd/doctor.rs
git commit -m "perf: contentless FTS5 + fts_map (v13) — zero body copies

The FTS layer previously stored the full body twice (fts_word_content +
fts_ngram_content on top of episodes.content, ~3x the text). Both tables
are now contentless (contentless_delete=1) with a tiny rowid map;
episodes.content is the single text copy. Also fixes compacted episodes
vanishing from search (content='' was indexed) and deletes orphan
source-registry rows (the tempdir-path leak)."
```

---

### Task 5: Document plane v2 — doc_texts + external-content FTS + int8 doc_vectors (B1 + B2, documents.db)

`doc_fts_word.body` is currently the canonical text store (read back by `pending_vector_chunks`), with `doc_fts_ngram` holding a second full copy. New shape: one real `doc_texts` table (the only body copy) + two **external-content** FTS5 indexes on it (queries keep reading `space`/`chunk_id` transparently — no query rewrite), + `doc_vectors` int8.

**Files:**
- Create: `crates/oxibrain-store/src/documents_v2.sql`
- Modify: `crates/oxibrain-store/src/documents.rs` — `migrate` chain (v1→v2), insert/delete ordering, `pending_vector_chunks`, vector upsert/read
- Modify: `crates/oxibrain-store/src/documents.rs` const `DOCUMENTS_SCHEMA_VERSION = 2`
- Test: extend the existing tests module in `documents.rs`

**Interfaces:**
- `documents_v2.sql`:

```sql
-- v2: single body copy + external-content FTS + int8 vectors.
CREATE TABLE IF NOT EXISTS doc_texts (
  rowid INTEGER PRIMARY KEY,
  body  TEXT NOT NULL,
  space TEXT NOT NULL,
  chunk_id TEXT NOT NULL UNIQUE
);

DROP TABLE IF EXISTS doc_fts_word;
CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts_word USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid'
);
DROP TABLE IF EXISTS doc_fts_ngram;
CREATE VIRTUAL TABLE IF NOT EXISTS doc_fts_ngram USING fts5(
  body, space UNINDEXED, chunk_id UNINDEXED,
  content='doc_texts', content_rowid='rowid',
  tokenize = 'trigram'
);

DROP TABLE IF EXISTS doc_vectors;
CREATE VIRTUAL TABLE IF NOT EXISTS doc_vectors USING vec0(
  chunk_id TEXT PRIMARY KEY,
  embedding int8[1024]
);
```

- Migration Rust step: copy bodies across **before** dropping the old tables — `INSERT INTO doc_texts(rowid, body, space, chunk_id) SELECT f.rowid, f.body, f.space, f.chunk_id FROM doc_fts_word f` (order the DDL accordingly: create doc_texts, copy, then drop/recreate the FTS tables, then `INSERT INTO doc_fts_word(doc_fts_word) VALUES('rebuild')` and same for ngram to index from the content table). `doc_vectors` rows are NOT converted (re-embed via `index --embed` after upgrade; document this in the migration comment and doctor output if trivial).
- Produces: same public API on `DocumentCache`. `pending_vector_chunks` reads `doc_texts` directly (`SELECT c.id, t.body FROM doc_chunks c JOIN doc_texts t ON t.chunk_id = c.id WHERE …`).

- [ ] **Step 1: Failing test** — v1 fixture db (old schema with 1 document + chunks + fts rows) → `DocumentCache::migrate` → assert: `doc_texts` has the same bodies; `sqlite_master` shows `content='doc_texts'` on both FTS tables; search via the existing `search` helper still returns the chunk; doc_vectors SQL contains `int8[1024]`.

- [ ] **Step 2: Implement** — `documents_v2.sql`, migrate chain (`if current < 2` branch mirroring the existing `if current < 1` shape), then in `documents.rs`:
  - Insert path (~line 1100): insert into `doc_texts(body, space, chunk_id)` first, capture `last_insert_rowid()`, then `INSERT INTO doc_fts_word(rowid, body, space, chunk_id) VALUES (?1,?2,?3,?4)` (external-content: indexes, does not store) + same for ngram.
  - Delete paths (~lines 872-1042): delete FTS rows FIRST (`DELETE FROM doc_fts_word WHERE chunk_id IN (…)` — external-content DELETE reads old values from `doc_texts`, which must still exist at that moment), THEN `DELETE FROM doc_texts WHERE chunk_id IN (…)`, then the existing `doc_chunks`/`documents` deletes.
  - `pending_vector_chunks` (~line 684): source bodies from `doc_texts`.
  - Vector upsert/read (`upsert_*` for doc_vectors): int8 quantization at the boundary, scale 1.0, same as Task 3.
- [ ] **Step 3: Tests** — existing tests in `documents.rs` assert row counts on `doc_fts_word`/`doc_fts_ngram` — they keep passing (row counts are identical for external-content tables). Add one asserting `doc_texts` holds exactly one body per chunk and that deleting a document empties `doc_texts` too.
- [ ] **Step 4: Run suite, commit**

```bash
cargo test -p oxibrain-store && cargo clippy -p oxibrain-store --all-targets -- -D warnings
git add crates/oxibrain-store/src/documents_v2.sql crates/oxibrain-store/src/documents.rs
git commit -m "perf: documents.db v2 — single body copy, external-content FTS, int8 vectors

doc_texts becomes the canonical text store (was: two full copies inside
the FTS content tables). FTS5 external-content keeps search SQL
unchanged. doc_vectors goes int8; embeddings are recomputed on the next
index --embed."
```

---

### Task 6: Workspace verification + measured proof

- [ ] **Step 1: Full gates**

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1 || true
```

- [ ] **Step 2: Ranking quality gates**

```bash
cargo test -p oxibrain-embed-local --test ranking_equivalence -- --ignored
cargo test -p oxibrain --test m2_index_determinism
cargo run -p oxibrain-cli -- eval --suite fast
```

Expected: determinism byte-identical (truth half); ranking equivalence within the §5.1 tolerance; eval deltas within noise on the cached corpus. If int8 moves recall@10 beyond tolerance, STOP and report the measured delta — do not silently widen the tolerance.

- [ ] **Step 3: Real-instance proof** — on a copy of `~/.oxi/brain` (`cp -r` to a tempdir, `--dir` it): run the CLI's migration-triggering command + `reproject` + `index --documents`, then:

```bash
sqlite3 <copy>/brain.db "SELECT name, printf('%d kb', SUM(pgsize)/1024) FROM dbstat WHERE name NOT LIKE 'vec_%' GROUP BY name ORDER BY 2 DESC LIMIT 12;"
sqlite3 <copy>/documents.db "SELECT COUNT(*), SUM(LENGTH(body)) FROM doc_texts;"
du -sh <copy>
```

Record before/after numbers in the task output: expected ≈ −736 KB fts content, tfidf ÷4, sources −720 rows on this instance.

- [ ] **Step 4: One-time junk deletion on the real instance**

```bash
rm ~/.oxi/brain/brain.db.pre-v11.bak ~/.oxi/brain/brain.db.pre-v11.wal.bak ~/.oxi/brain/daemon.log
```

(Manual migration remnants from 2026-08-27 + the retired daemon's log. No code writes these paths anymore.)

---

### Task 7: Documentation

- [ ] **Step 1: ADR** — `doc/adr/2026-08-28-storage-footprint.md`: contentless FTS + fts_map vs external-content (brain has no single content table; doc does), int8 symmetric quantization (scale-invariance argument, no metadata column), junk-path elimination (orphan sources, quarantine-as-retry-queue), the rejected bit-quantization option (no rescore path without keeping f32 originals).
- [ ] **Step 2: ARCHITECTURE.md** — bump version header; §7.4 note the storage budget now measured (trigram data 3.4×, zero body copies, int8 vectors); §5.7 chunk text recovery unchanged; F-series add findings (orphan sources, quarantine growth, compacted-episode FTS bug — mark fixed); §17.1 note v12/v13 and documents v2.
- [ ] **Step 3: CHANGELOG** entry + commit `docs: storage footprint — contentless FTS, int8 vectors, junk-path elimination (ADR)`.
