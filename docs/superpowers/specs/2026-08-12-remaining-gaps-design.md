# oxibrain Remaining Gaps — Design Spec

> **Status:** Draft — awaiting user review.
> **Context:** Post-M6 gap analysis (2026-08-12). oxibrain's knowledge model
> (ledger, temporal fold, extraction, security, MCP) is shipped and tested.
> This spec covers the gaps between the shipped system and DESIGN.md's stated
> targets, prioritized by product impact.

## Scope decomposition

The remaining work spans three independent subsystems. Each is a standalone
spec → plan → implementation cycle.

| ID | Sub-project | Impact | Size | Blocks |
|---|---|---|---|---|
| **R** | Retrieval Infrastructure — dense embeddings, HNSW, sqlite-vec, community/graph modes | 🔴 Critical — DESIGN §9.1 promises four retrieval modes; only 1.5 work | Large (3-5 days) | Nothing |
| **Q** | Quality Wiring — salience, confidence, frontend API bugs, token CSPRNG | 🔴 Critical — UI is broken, security is weak, ranking signals are absent | Medium (1-2 days) | Nothing |
| **L** | Lifecycle Automation — background extractor, consolidation checkpointing, proactive recall | 🟡 Important — DESIGN §7.6/§10; agent self-sufficiency | Medium (1-2 days) | R (partially) |

**Dependency:** Q is independent and should ship first (unblocks the UI, fixes
security). R is the largest and most impactful. L depends on R for embedding-
based proactive recall but can ship its non-embedding parts independently.

**DESIGN §16.2 dispositions honored:** SONA stays in oxios (agent runtime
concern). `auto_bridge`, `auto_classify`, `auto_protect` stay in oxios.
`hyperbolic`, `flash_attention`, `embedding_viz` are deferred (unproven).
`graph` (PageRank) is adopted as a salience signal only. `root_index`/`quota`
are re-scoped to salience. `proactive` folds into `assemble_context`.

---

## Sub-project Q: Quality Wiring

### Problem

Four independent defects that each block real usage:

1. **Frontend↔server API contract mismatches (3 calls completely broken).** The
   React UI sends arguments the MCP server cannot parse. GraphExplorer,
   ContradictionInbox retract, and mergeEntities all fail silently.

2. **Token secret generated with non-CSPRNG entropy.** `generate_secret()` uses
   `std::collections::hash_map::RandomState` (SipHash, seeded from OS entropy
   but not a CSPRNG). Bearer tokens gate a remote store; this is predictable
   in principle.

3. **Salience hardcoded `1.0` in retrieval.** The decay function exists
   (`lifecycle.rs::salience`) and is applied to the `entities.salience` column
   by `apply_decay`, but `query.rs` and `retrieval.rs` ignore it — every result
   gets `salience: 1.0`. The ranking signal that DESIGN §9.2 calls out ("decay
   and access frequency are ranking signals only") is dead.

4. **Confidence hardcoded `1.0` in fold.** `fold.rs` sets `confidence: 1.0`
   for every belief. The formula in DESIGN §6.5 and the types in
   `confidence.rs` exist but are not wired. Retrieval's `min_confidence`
   filter in `TraversalSpec` is therefore a no-op.

### Design

#### Q1: Frontend API contract fix

The server is the source of truth — the frontend adapts. Three changes in
`apps/brain-ui/src/api.ts`:

| Tool | Current (broken) | Fix → match server |
|---|---|---|
| `traverse` | `{ start_entities: [...] }` | `{ start: [...] }` |
| `retract` | `{ entity_id, predicate }` | `{ subject: { surface, type }, predicate, object: { kind, value }, episode }` |
| `merge_entities` | `{ canonical_entity_id, merged_entity_id }` | `{ loser: { surface, type }, winner: { surface, type } }` |

The `retract` and `merge_entities` calls need richer arguments (EntityRef with
surface + type, DeclObject with kind + value, episode ID). The
ContradictionInbox must supply these from the contradiction data. If the
contradiction response doesn't include enough information (entity type,
episode ID), the server's `contradictions` tool response must be enriched.

**Approach:** Fix the arg names first (unblocks the happy path). Then evaluate
whether the contradiction response has enough data for full retract/merge
calls, or whether the server needs a simpler `resolve_contradiction` tool that
takes a `statement_id` and a resolution action.

#### Q2: Token secret CSPRNG

Replace `RandomState` with `getrandom` (already a transitive dependency via
many crates, or add it directly — it's 2KB, no_std-compatible, and the Rust
standard for OS entropy).

```rust
fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("getrandom failed");
    format!("obt_{}", hex::encode(&bytes))
}
```

No new heavy dependency. `getrandom` is maintained by the RustCrypto working
group and used by `rand` itself.

#### Q3: Salience wiring

The salience value lives in `entities.salience` (a `REAL` column, updated by
`apply_decay` and `rebuild_salience`). Wire it into:

1. **`query.rs::hybrid_query`** — after RRF fusion, multiply the final score
   by `salience` from the entity row (already joined). The `RankedItem.salience`
   field gets the real value, not `1.0`.

2. **`query.rs::traverse`** — same: `TraversalNode.salience` reads from the
   entity row.

3. **`store/context.rs::assemble_context`** — the `HighSalienceBeliefs` layer
   should actually filter by salience threshold, not just select all beliefs.

**Approach:** Simple — the column exists, the function exists, the types carry
the field. It's three `1.0` → `row.get("salience")` replacements plus making
sure `apply_decay` is called periodically (wired to the consolidation window
or a CLI command, which already exists: `oxibrain stats` triggers it
indirectly).

#### Q4: Confidence wiring

Wire `confidence.rs::compute_confidence` into `fold.rs`. The fold currently
sets `confidence: 1.0`; it should call the formula with:
- `extractor_id` → `calibrate(extractor)` — for v1, a conservative `0.8` prior
  unless the eval harness has measured it (stored in a `meta` key).
- `corroboration` — count of distinct episodes affirming the belief (available
  from the assertion set being folded).
- `trust` — weighted by `Episode.trust` (already on each assertion's source
  episode).
- `recency_of_support` — for `Interval` predicates only, based on
  `claimed_from` proximity to now.

**Approach:** The formula is pure and already implemented in `confidence.rs`.
The fold has all the inputs. The wiring is: after computing the merged
intervals, compute confidence per interval from the assertions that survived
into it. Manual declarations (`Declaration` episodes) bypass at `1.0` per
DESIGN §6.5.

**Risk:** Changing confidence affects every belief row. The reprojection test
must still pass byte-identically — which it will, because confidence is a
deterministic function of the assertion set, and the assertion set is
deterministic. The test fixture's expected values will change, but the
determinism property holds.

---

## Sub-project R: Retrieval Infrastructure

### Problem

DESIGN §9.1 specifies four retrieval modes: `lexical` (FTS5/BM25 ✅),
`semantic` (vector kNN — **only TF-IDF brute-force**), `graph` (bounded
traversal — **fake, no neighbor expansion**), `community` (map-reduce over
summaries — **dead code, returns empty**).

The `EmbeddingPort` trait exists with zero adapters. No HNSW. No sqlite-vec.
At 10⁵ episodes, brute-force kNN cannot meet the §13.2 budget.

### Approaches considered

**Approach A: Full adoption of oxios-memory's modules (per §16.2).**
Port `hnsw.rs`, `embedding.rs`, `sqlite/search/vector.rs` from oxios-memory
into `oxibrain-index` and `oxibrain-store`. Pro: battle-tested code. Con:
different schema, different types, different error handling — effectively a
rewrite against oxibrain's contracts. DESIGN §16.2 says "adopt as reference,
rewritten against the new schema."

**Approach B: Fresh implementation guided by DESIGN + oxios as reference.**
Implement the embedding port adapter, HNSW, and sqlite-vec integration from
scratch, using oxios-memory code as a reference for algorithm details but not
copying it. Pro: clean integration with oxibrain's port boundaries, no type
mismatches. Con: more code to write. **This is the DESIGN-sanctioned path.**

**Recommendation: Approach B.** DESIGN §16.2 explicitly says "rewritten
against the new schema." The oxios code is useful as algorithm reference but
the types, error handling, and store integration are incompatible.

### Design

#### R1: Dense embedding port adapter

```
oxibrain-ports:  EmbeddingPort (trait — exists, empty)
                         ↑
oxibrain-index:  TfidfAdapter (exists)
                 DenseAdapter (NEW — wraps a GGUF model)
                         ↑
              GGUF model (aarch64, feature-gated)
```

**`DenseAdapter`** implements `EmbeddingPort` using a GGUF model loaded via
`llama-gguf` (the same crate oxios uses). Feature-gated as `embedding-gguf`
(aarch64 only — same constraint as oxios). The adapter produces dense `f32`
vectors of configurable dimension (default 384 for all-MiniLM-L6-v2, the
smallest model that works well).

**TF-IDF remains the default** (zero-dependency, works offline). The dense
adapter is opt-in. DESIGN §20 open question 1 says: "Default: TF-IDF works
offline immediately; offer a dense-model download on first `ingest`."

**No model bundling.** The binary stays small. First use of `--embedding gguf`
triggers a download prompt (or uses a path from config). This matches
DESIGN §20.1.

#### R2: sqlite-vec persistent vector store

Add a `vec0` virtual table to `oxibrain-store`:

```sql
-- Migration v5
CREATE VIRTUAL TABLE entity_vectors USING vec0(
    entity_id TEXT PRIMARY KEY,
    embedding FLOAT[N]
);
```

`N` is fixed at store-creation time (stored in `meta`). The writer actor owns
all inserts/deletes. Readers query via `SELECT ... ORDER BY distance LIMIT k`.

**Integration with reprojection:** vectors are projection (derived from entity
text + embedding model). `reproject` rebuilds them by re-embedding every
entity. This is expensive (one model call per entity) but deterministic and
rare. The embedding model ID is part of the reprojection state (stored in
`meta`), so reprojection with a different model produces different vectors —
which is correct, not a bug.

#### R3: HNSW in-memory index

Implement in `oxibrain-index::hnsw` (pure Rust, no `rusqlite`). The HNSW
index is an in-memory cache over the `entity_vectors` table, built lazily on
first query and rebuilt on `reproject`.

**DESIGN §4.3 warning:** "Two processes with independent in-memory HNSW
indexes writing one SQLite file is a corruption path." This is already solved
by P8 (single writer, advisory lock). The daemon owns the HNSW index;
read-only connections use sqlite-vec KNN directly (no in-memory index needed
for read-only queries — sqlite-vec is fast enough for point queries).

#### R4: Hybrid query wiring

`query.rs::hybrid_query` currently fuses lexical (FTS5) + semantic (TF-IDF
kNN). Upgrade to:

1. **Lexical:** FTS5 BM25 (unchanged).
2. **Semantic:** sqlite-vec KNN (if vectors exist) or TF-IDF kNN (fallback).
3. **Graph:** BFS neighbor expansion from seed entities (the `traverse` code
   already does this correctly — wire it into the hybrid path as a ranking
   signal: entities closer to query-relevant entities get boosted).
4. **Community:** if the query has no entity anchor, fall back to community
   summaries. Map the query to communities via their summary embeddings, then
   return community members ranked by intra-community salience.

RRF fuses all available modes. Each mode contributes a ranked list; RRF
combines them. Modes that are unavailable (no dense embeddings, empty
communities) simply don't contribute — the hybrid result degrades gracefully.

#### R5: QueryMode::Community

Community label propagation already runs (`communities.rs`, deterministic).
The query path needs to:

1. Embed the query (TF-IDF or dense).
2. Match against community centroid vectors (mean of member entity vectors).
3. Return top-k communities with their members, sorted by member salience.

For v1, community summaries (DESIGN §9.4) require LLM text generation, which
is M3 territory and already implemented (`summarize_communities`). The query
path uses the summary text for FTS5 matching if summaries exist, or falls back
to member-entity matching if they don't.

#### R6: QueryMode::Graph fix

The current graph mode re-tags FTS5 hits. Fix: when `QueryMode::Graph` is
selected, seed the traversal from lexical hits (top-k FTS5 entity matches),
then expand via BFS to depth 2. The traversal results replace the raw FTS5
hits. This gives multi-hop recall without changing the query interface.

---

## Sub-project L: Lifecycle Automation

### Problem

Three lifecycle features specified in DESIGN but not automated:

1. **Background extractor** (§7.6): "Extraction is queued and rate-limited,
   never synchronous with a user write unless `mode: sync`." Currently, plain
   `ingest` inserts an episode only; extraction is a manual `extract` step.
   Nothing drains the `ingest_jobs` queue automatically.

2. **Consolidation checkpointing** (§10): The `consolidate()` method works but
   has no checkpoint/recovery. A crash mid-consolidation loses progress. The
   oxios Dream process has checkpointing; DESIGN §16.2 says "adopt, redesigned
   — emits derived episodes instead of calling forget()."

3. **Proactive recall** (§16.2: "folds into assemble_context"): DESIGN §9.5
   specifies `assemble_context(query, token_budget)` as the single recall
   primitive. The current implementation is functional but doesn't
   auto-trigger — agents must call it explicitly. The oxios `ProactiveRecall`
   module has a 3-step selective recall with topic-change detection.

### Design

#### L1: Background extraction worker

Add a background task to the daemon that periodically calls
`brain.extract_pending()`. Configurable interval (default 60s), budget
(max episodes per batch, max concurrent LLM calls, max spend/day). The worker
respects the extraction budget types already defined in
`ExtractionBudget`.

**Approach:** A tokio interval task in the daemon's main loop. In embedded
mode, the application owns the scheduling (call `extract_pending` on its own
timer). The daemon worker is opt-in via config (`extraction.worker_interval`).

**Not a thread pool.** The single-writer actor (P8) serializes all writes.
The worker claims jobs (atomic SQL update), processes them (LLM call outside
transaction per §7.2), then writes results (short transaction). Multiple
LLM calls can be in-flight concurrently — only the write-back is serialized.

#### L2: Consolidation checkpointing

The consolidation process (`consolidate()`) already:
1. Finds episode clusters
2. Generates summaries (LLM, cached)
3. Writes Derived episodes

Add checkpointing:
- Before processing a cluster, write a `consolidation_checkpoints` row
  (`cluster_hash`, `status`, `started_at`).
- After writing the Derived episode, mark it `completed`.
- On restart, skip completed clusters and resume in-progress ones.

**Table:**
```sql
-- Migration v5 (same migration as vectors, or v6)
CREATE TABLE consolidation_checkpoints (
    cluster_hash TEXT PRIMARY KEY,
    extractor_id TEXT NOT NULL,
    status TEXT NOT NULL,         -- 'in_progress' | 'completed' | 'failed'
    started_at INTEGER NOT NULL,
    completed_at INTEGER
);
```

This is a cache table (not ledger) — `reproject` ignores it and rebuilds
from scratch. Its purpose is crash recovery for long consolidation runs.

#### L3: Proactive recall via assemble_context

DESIGN §16.2 says `proactive` "folds into `assemble_context`." The current
`assemble_context` is query-driven (takes a query string). Proactive recall
adds:

1. **Topic-change detection** — when consecutive queries differ significantly
   (keyword overlap below threshold), trigger a broader context assembly that
   includes community summaries and recent episodes.

2. **Session-start context** — first query in a session gets a richer context
   (pinned facts + high-salience beliefs + recent episodes without a specific
   query anchor).

**Approach:** Add an optional `RecallHints` parameter to `assemble_context`:
```rust
pub struct RecallHints {
    pub is_session_start: bool,
    pub topic_changed: bool,
    pub recent_queries: Vec<String>,
}
```

When `topic_changed` or `is_session_start`, the context assembler widens the
net: includes community summaries, more recent episodes, higher token budget
for the neighborhood layer. When neither, it behaves as today (tight,
query-anchored).

**This is an API addition, not a change.** Callers that don't pass hints get
the current behavior. The MCP `recall` tool gains optional hint parameters.

**Not auto-triggered from the server.** The client (agent runtime, CLI) decides
when to call `recall` with hints. The server doesn't push — that's the
deferred subscriptions feature (ADR-001).

---

## Migration plan

All schema changes land in **migration v5**:

```sql
-- v5: Embedding vectors + consolidation checkpoints
CREATE VIRTUAL TABLE entity_vectors USING vec0(
    entity_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);

CREATE TABLE consolidation_checkpoints (
    cluster_hash TEXT PRIMARY KEY,
    extractor_id TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    completed_at INTEGER
);

INSERT INTO meta (key, value) VALUES
    ('embedding_dim', '384'),
    ('embedding_model', ''),
    ('schema_version', '5');
```

**Up-test fixture:** v4 fixture (existing) → apply v5 → assert new tables
exist and `entity_vectors` accepts inserts.

**Reprojection:** vectors are projection. `reproject()` drops and rebuilds
`entity_vectors` by re-embedding every entity. The embedding model is stored
in `meta`; reprojection with no model set (TF-IDF default) skips the vector
table — TF-IDF vectors live in `tfidf_vectors` (existing).

---

## Priority and sequencing

```
Q (Quality Wiring)     ──→ ship first (1-2 days)
  ├─ Q1: Frontend API fix (2h)
  ├─ Q2: Token CSPRNG (30m)
  ├─ Q3: Salience wiring (2h)
  └─ Q4: Confidence wiring (4h)

R (Retrieval Infra)    ──→ ship second (3-5 days)
  ├─ R1: Dense embedding adapter (1d)
  ├─ R2: sqlite-vec table + migration (4h)
  ├─ R3: HNSW in-memory index (1d)
  ├─ R4: Hybrid query wiring (4h)
  ├─ R5: Community query mode (4h)
  └─ R6: Graph query mode fix (2h)

L (Lifecycle Auto)     ──→ ship third (1-2 days)
  ├─ L1: Background extractor (4h)
  ├─ L2: Consolidation checkpointing (3h)
  └─ L3: Proactive recall hints (3h)
```

**Commit strategy:** Each sub-project is a feature branch
(`feat/quality-wiring`, `feat/retrieval-infra`, `feat/lifecycle-auto`).
Squash-merge to main. Each sub-project gets its own implementation plan
(via writing-plans skill) before code.

---

## What this spec deliberately excludes

Per DESIGN §16.2 dispositions:

- **SONA pattern engine** — stays in oxios (agent behavior learning is a
  runtime concern, not a knowledge-graph concern).
- **auto_bridge / auto_classify / auto_protect** — stay in oxios (agent-
  runtime glue). oxibrain's extraction is LLM-based, not heuristic.
- **hyperbolic / flash_attention / embedding_viz** — deferred (unproven, not
  on the v1 path).
- **Long-running tasks / subscriptions** — remain deferred per ADR-001 (MCP
  protocol features for third-party consumers; polling-based UX works).
- **5-level compaction hierarchy** — oxibrain's compaction is simpler by
  design (episode-content BLOB compression, not a multi-level summary tree).
  The compaction hierarchy in oxios solved a problem oxibrain doesn't have
  (flat text store vs. ledger+projection).
- **RootIndex TOC** — oxibrain's community summaries serve the same purpose
  (thematic overview of what the brain knows). A separate RootIndex is
  redundant.
- **PageRank salience** — DESIGN §16.2 says "adopt as salience signal only."
  This is a v2 enhancement; the current salience decay formula is sufficient
  for v1.
