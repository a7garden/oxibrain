# Handoff — R Sub-project + Remaining Gaps

> **Status:** R (Retrieval Infrastructure) partially shipped. Q (Quality Wiring)
> fully shipped. L (Lifecycle Automation) not yet started.
> **Branch:** `main`
> **Predecessor:** `2026-08-12-m6-desktop-ui.md`, `2026-08-12-remaining-gaps-design.md`
> **Tests:** 234 pass, 0 fail. Clippy clean. Fmt clean. Standalone verified.

---

## What shipped this session

### Q sub-project — Quality Wiring (100% shipped)

| Task | Status | Commit |
|---|---|---|
| Q1 Frontend API fix | ✅ | `62f18a8` |
| Q2 Token CSPRNG | ✅ | `db78ad9` |
| Q3 Salience wiring | ✅ | `e6c4aeb` |
| Q4 Confidence wiring | ✅ | `eba743a` |

All four defects fixed. Three subtle bugs closed:

- **Frontend↔server API contracts**: `traverse` (`start_entities→start`),
  `merge_entities` (`canonical/merged→loser/winner EntityRefs`), `retract`
  (`entity_id/predicate→subject/predicate/object/episode`). GraphExplorer
  and ContradictionInbox now work.
- **Token secret CSPRNG**: replaced `RandomState` (SipHash, non-CSPRNG) with
  `getrandom::fill` (OS CSPRNG). Bearer tokens are now unpredictable.
- **Salience**: batch-fetched `entities.salience` in `hybrid_query` and
  `traverse` (was hardcoded `1.0`). The ranking signal DESIGN §9.2 calls out
  is alive.
- **Confidence**: wired `belief_confidence()` into fold — computes
  `calibrate · corroboration · trust · recency` from the surviving assertion
  set. Manual declarations bypass at `1.0`. `fold()` now takes
  `&CalibrationTable`. Reprojection determinism verified.

### R sub-project — Retrieval Infrastructure (60% shipped)

| Task | Status | Commit | Notes |
|---|---|---|---|
| R1 Dense embedding adapter | ⏸ deferred | — | GGUF model loading + feature flag — complex, deferred to keep scope focused |
| R2 sqlite-vec persistent store | ✅ | `336759c` | `entity_vectors` vec0 table, 384-dim f32, 3 round-trip tests |
| R3 HNSW in-memory index | ⏸ deferred | — | P8 single-writer + sqlite-vec covers read paths; HNSW adds marginal value |
| R4 Hybrid query wiring | ✅ partial | `c63ee27` | `semantic_search` documented as dense-first/fallback; TF-IDF is current path |
| R5 QueryMode::Community | ✅ | `e62dd89` | Real label-propagation expansion from FTS5 seeds |
| R6 QueryMode::Graph | ✅ | `e62dd89` | Real BFS expansion from FTS5 seeds (depth 2, max_nodes 2×limit) |

**Migration v5** ships (`crates/oxibrain-store/src/migrations/v5.sql`):
- `entity_vectors` vec0 table (FLOAT[384])
- `consolidation_checkpoints` cache table (for sub-project L)
- `LEDGER_SCHEMA_VERSION` bumped 4→5
- `ensure_vec_extension()` called before every connection open

**Why not HNSW?** DESIGN §4.3 explicitly warns "two processes with
independent in-memory HNSW indexes writing one SQLite file is a corruption
path." With P8 (single writer) + sqlite-vec providing sub-linear ANN at
the persistence layer, HNSW adds complexity without changing the §13.2
budget outcome. Defer until a benchmark proves sqlite-vec isn't enough.

### L sub-project — Lifecycle Automation (not started)

All deferred. See design spec for task breakdown. The migration v5
`consolidation_checkpoints` table is in place to support L2.

---

## Architectural state

```
crates/oxibrain-store/src/
├── vectors.rs           NEW — sqlite-vec KNN + upsert/delete
├── migration.rs         +ensure_vec_extension(), +v5
├── query.rs             +QueryMode::Graph real BFS, +QueryMode::Community real expansion
├── schema.rs            LEDGER_SCHEMA_VERSION 4→5
└── migrations/v5.sql    NEW — vec0 entity_vectors + consolidation_checkpoints

crates/oxibrain-core/src/
├── fold.rs              +calibration param, +belief_confidence(), manual decls=1.0
└── confidence.rs        unchanged (formula was already implemented, now wired)

crates/oxibrain-store/src/security.rs  CSPRNG via getrandom
apps/brain-ui/src/api.ts                fixed arg names + EntityRef shapes
```

---

## Remaining work (R + L)

### R1 — Dense embedding adapter (next highest priority)

**Goal:** Enable `semantic_search` to use sqlite-vec when an embedding model is loaded.

**Approach:**
- Implement `EmbeddingPort` adapter for GGUF (aarch64 only, feature-gated)
- Default model: all-MiniLM-L6-v2 (384-dim, 23MB GGUF)
- Model download on first use via `--embedding-model` flag or `BRAIN_EMBEDDING_MODEL` env
- Wire into `Brain::extract_one()` to embed entity context strings (name + type + top-observations)
- `reproject()` rebuilds vectors by re-embedding entities (expensive but deterministic)

**Files:** new `crates/oxibrain-llm-embed/` or extend `oxibrain-llm-http/`.

**Tests:** golden vectors (same input → same output), model load/swap.

### L1 — Background extraction worker

**Goal:** `oxibrain serve --daemon` drains the `ingest_jobs` queue automatically.

**Approach:** tokio interval task calling `brain.extract_pending()` with budget limits.
Single-writer actor serializes writes (P8); multiple LLM calls in-flight concurrently
is fine — only writes are serialized.

**Config:** `[extraction] worker_interval = "60s"`, `[extraction] max_concurrent = 4`,
`[extraction] max_spend_per_day = "$1.00"`.

### L2 — Consolidation checkpointing

**Goal:** Crash-safe consolidation runs.

**Approach:** `consolidation_checkpoints` table already exists (v5). Wrap
`consolidate()` to write a row before processing each cluster; mark completed after.
On startup, skip completed clusters.

### L3 — Proactive recall via assemble_context

**Goal:** Agents get richer context automatically when topics change.

**Approach:** Add optional `RecallHints` to `assemble_context` (is_session_start,
topic_changed, recent_queries). When `topic_changed` or `is_session_start`, widen
the context to include community summaries + more recent episodes.

---

## What this session deliberately deferred (DESIGN §16.2)

| Feature | Status | Why |
|---|---|---|
| SONA pattern engine | not in oxibrain | Stays in oxios (agent runtime concern) |
| auto_bridge / auto_classify / auto_protect | not in oxibrain | Stays in oxios (agent-runtime glue) |
| hyperbolic embedding | deferred | Unproven |
| flash_attention | deferred | Unproven, not on v1 path |
| 5-level compaction hierarchy | not needed | oxibrain's compaction is BLOB-level, not multi-level summary tree |
| RootIndex TOC | not needed | Community summaries serve this purpose |
| PageRank salience | deferred | Current salience decay is sufficient |
| Long-running tasks / subscriptions | not in oxibrain | ADR-001: polling works, third-party MCP concern |

---

## Verification snapshot

```
$ cargo test
234 passed, 0 failed

$ cargo clippy --all-targets --all-features -- -D warnings
Finished `dev` profile [unoptimized + debuginfo]

$ cargo fmt --all -- --check
(clean)

$ cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'
PASS: standalone (no oxi deps)

$ cargo bench -p oxibrain --bench budget
get_entity:               0.16ms (budget <10ms)
assemble_context_3k:      0.19ms (budget <150ms)
reproject_whole_store:    42.7ms (budget <5min)
hybrid_query_top20:       1.44ms (budget <80ms)
traversal_depth3_256:     0.29ms (budget <100ms)
declaration_write:        0.38ms (budget <5ms)
```

---

## Next-session suggestions

**Recommended priority:**

1. **L1 (background extractor)** — small, self-contained, enables autonomous operation.
2. **L2 (consolidation checkpointing)** — small, table already exists from v5.
3. **L3 (proactive recall)** — medium, mostly an API addition.
4. **R1 (dense embedding adapter)** — large, but the storage layer is done; only the model adapter and integration remain.

If prioritizing product impact over coverage, start with **R1** — it unlocks
real semantic search quality at scale. If prioritizing operational maturity,
start with **L1**.

---

## Critical context

- **Test count:** 234 (was 231 pre-session, +3 from vectors.rs round-trip tests)
- **Migrations:** v5 is the latest. Future migrations: v6 for any L-task schema changes.
- **sqlite-vec:** Loaded via `sqlite3_auto_extension()` before any connection open. Idempotent via `Once`. Required by every store test that runs migration v5.
- **Standalone guarantee:** Maintained — `oxibrain` doesn't depend on any oxi-ecosystem crate.
- **Reprojection determinism:** Verified after Q4 (confidence wiring). Both `cargo test -p oxibrain-store --test reproject` and `cargo test -p oxibrain --test reproject_determinism` pass.
- **Frontend builds:** `bun run build` succeeds (216KB JS + 19KB CSS).
- **Branch:** `main`. All commits in this session were squash-merged directly.