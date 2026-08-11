# oxibrain M2 — Retrieval & Lifecycle Design Spec

> **Date:** 2026-08-11
> **Authority:** `doc/DESIGN.md` v1.0 (§§9, 10, 12.1, 13.2–13.3, 14.3, 15, 17). This spec
> scopes and concretizes M2. Where this spec and DESIGN.md disagree, DESIGN.md wins
> unless this spec explicitly records a deviation (§15).
> **Predecessor:** M1 Knowledge Core (complete — see
> `docs/superpowers/specs/2026-08-11-oxibrain-m1-knowledge-core-design.md`).
> **Status:** Design. Drives the M2 implementation plan.

---

## 1. Goal

The retrieval and lifecycle layer: lexical (FTS5/BM25), semantic (TF-IDF kNN),
graph (adjacency traversal), and community (label propagation) retrieval fused
with Reciprocal Rank Fusion; bounded traversal; salience decay and compaction;
context assembly; explainability queries (`timeline`, `diff`, `why`, `why
--dropped`); and the committed bench suite measuring the §13.2 performance
budgets. **Still deterministic — no LLM, no dense embeddings, no network.**

## 2. M2 Exit Criteria (DESIGN §17)

1. Budgets measured and either met or revised **once** with recorded evidence (§14).
2. Multi-hop (`traverse`) and thematic (community) queries answer over a
   hand-built graph (§13).
3. M1 exit criteria still hold: fold property tests pass; reprojection is
   byte-identical; `as_of` and contradiction queries still answer (§12).

---

## 3. Scope

### 3.1 In M2

| Capability | Detail |
|---|---|
| FTS5/BM25 lexical index | Virtual table over episode content + statement renderings; BM25 ranking |
| TF-IDF semantic baseline | Deterministic TF-IDF vectors (hashing trick, fixed D=1024); cosine kNN |
| KNN index (in-memory) | Deterministic kNN over TF-IDF vectors; brute-force cosine for M2 scale (HNSW optimization deferred — §15 D4) |
| Adjacency graph | Pure graph view over statements (subject→object edges); belief-filtered |
| Hybrid query | RRF fusion (Cormack 2009) of lexical + semantic + graph ranked lists |
| Bounded traversal | `TraversalSpec` (§9.3): BFS with depth/nodes/predicate/direction caps |
| Community clustering | Label propagation — deterministic (fixed cap + id tie-break); summary **text** deferred to M3 |
| Salience | Time-decay score derived from assertion timestamps; pure fn; cached column rebuilt by reprojection |
| Decay | Periodic WriteOp that recalculates cached salience (deterministic formula) |
| Compaction | Moves cold episode content to a compressed BLOB column; keeps row/hash/links |
| `assemble_context` | Packing policy: pinned facts + high-salience beliefs + neighborhood + recent episodes → token budget |
| `timeline` | Belief intervals for an entity over a `[from, to]` range |
| `diff` | What changed for an entity at/between two time points |
| `why` | Provenance + confidence breakdown for a statement |
| `why --dropped` | What was filtered during a query and why (§13.3 instrument-discarded) |
| Bench suite | criterion benchmarks for every §13.2 budget, against a deterministic synthetic fixture |
| Index rebuild | Indexes rebuilt deterministically by reprojection (P1) |

### 3.2 Deferred to M3/M4

| Deferred | Milestone | Why |
|---|---|---|
| Dense GGUF embeddings (`oxibrain-embed-local`) | M3 | No LLM/dense model in M2; TF-IDF is the offline baseline |
| sqlite-vec persistence | M3 | TF-IDF vectors use BLOB storage (§15 D3); sqlite-vec arrives with dense embeddings needing native vector SQL |
| HNSW approximate kNN | M3 | Brute-force cosine kNN is deterministic and adequate at M2 scale; HNSW is an optimization for 10⁵+ dense vectors |
| Community summary **text** | M3 | Summary text is LLM-generated; clustering is deterministic (M2), text is cached `Derived` (M3) |
| Confidence calibration | M3 | Confidence is a 1.0 placeholder (M1); ranking uses salience, not confidence |
| Co-access PageRank salience signal | M3+ | §9.2 mentions PageRank/co-access as a salience signal; M2 uses time-decay only (deterministic from ledger) |
| MCP server, CLI full surface, connectors | M4 | Surfaces milestone |
| Spaces/scopes/tokens/audit/trust/redaction | M4 | Security/tenancy milestone |

### 3.3 What "still no LLM" means for M2

Primary episodes are ingested but produce **no assertions** (extraction is M3).
Only **Declaration episodes** produce entities, statements, assertions, and
beliefs. Therefore:

- FTS5 indexes episode **content text** (the raw text of primary + declaration
  episodes) and **statement renderings** (canonical `subject predicate object`
  strings derived from the projection).
- TF-IDF vectors are built over the same text corpus.
- Adjacency is derived from declaration-produced statements only.
- Communities cluster the declaration-produced entity graph.

The retrieval engine is fully functional over manually-declared knowledge. M3
wires extraction output into the same indexes with zero retrieval changes.

---

## 4. Architecture

### 4.1 Dependency DAG

M1 established: `ports ← core ← store ← oxibrain` (facade). Core is pure
(types + logic); store depends on core and orchestrates persistence.

M2 adds `oxibrain-index` as a new workspace member between core and store:

```
ports          ← base types: Timestamp, BrainError, ClockPort
  ↑
core           ← types + pure logic: knowledge types, fold, Query/TraversalSpec/
                  RankingResult types, salience formula, DecayConfig
  ↑
index          ← pure algorithms: RRF, TF-IDF, KnnIndex, AdjacencyGraph,
                  LabelPropagation — depends on core (type aliases) + ports.
                  NO rusqlite, NO I/O.
  ↑
store          ← persistence + execution: FTS5/sqlite queries, index orchestration,
                  traversal execution, decay/compaction WriteOps, communities
                  — depends on core + index + ports. Only rusqlite user.
  ↑
oxibrain       ← facade: async Brain wrapping store ops
```

**Why index is between core and store (not a sibling):** index's algorithms
(RRF, TF-IDF, kNN, adjacency, label propagation) consume core's type aliases
(`EntityId`, `StatementId`, `PredicateRef`). Store calls index algorithms during
projection writes (index update) and query execution (fusion, kNN). This gives a
clean linear DAG with no cycles. DESIGN §15 says "core may depend on store,
index, ports" — in M2, core stays pure (types + formulas only) and store
orchestrates, consistent with M1. Core gaining store/index dependencies is an
M5+ concern (when core becomes the full orchestrator).

**Why index does NOT reference rusqlite:** DESIGN §15: "Only `oxibrain-store`
may reference `rusqlite`." Index provides pure algorithms over in-memory data
structures. Store bridges rusqlite rows ↔ index structures.

### 4.2 Retrieval execution flow

```
Brain::query(Query::hybrid("auth decision"))
  → spawn_blocking → readers.read
    → store::query::hybrid_query(conn, space, query)
      1. lexical:  fts_search(conn, space, text, limit)        → Vec<SearchHit>
      2. semantic: tfidf_knn(conn, space, query_vec, limit)    → Vec<SearchHit>
      3. graph:    adjacency_neighbors(conn, space, seeds)     → Vec<SearchHit>
      4. fuse:     index::rrf::fuse([lexical, semantic, graph]) → Vec<RankedItem>
      5. enrich:   attach beliefs, provenance, explain          → RankingResult
```

Steps 1–3 are store functions (rusqlite). Step 4 is a pure index function.
Step 5 is store (joins back to projection). The query_vec in step 2 is computed
by `index::vector::tfidf_query_vector(text, vocab)` — a pure fn called by store
before the kNN lookup.

### 4.3 Module map

```
oxibrain-index/src/            # NEW crate
  lib.rs                       # index — re-exports
  rrf.rs                       # fuse(lists, k=60) → Vec<RankedItem>
  vector.rs                    # TfIdfModel, tfidf_vector, cosine_sim, tokenize
  knn.rs                       # KnnIndex: insert, search(k) — brute-force cosine (M2)
  adjacency.rs                 # AdjacencyGraph: add_edge, neighbors, bfs(spec)
  community.rs                 # label_propagation(graph, cap, tie_break) → CommunityMap

oxibrain-core/src/
  retrieval.rs                 # NEW — Query, QueryMode, TraversalSpec, Strategy,
                               #         Direction, PredicateFilter, RankingResult,
                               #         RankedItem, SearchHit, ExplainBlock
  lifecycle.rs                 # NEW — DecayConfig, salience(now, last_activity), 
                               #         CompactionConfig, SalienceEntry
  context.rs                   # NEW — ContextBudget, ContextLayer, ContextResult,
                               #         estimate_tokens

oxibrain-store/src/
  query.rs                     # EXTEND — hybrid_query, fts_search, tfidf_knn,
                               #           adjacency_neighbors, traverse
  timeline.rs                  # NEW — timeline, diff
  explain.rs                   # NEW — why, why_dropped
  index.rs                     # NEW — rebuild_indexes(conn, space): FTS5 + TF-IDF +
                               #           KNN load; update_on_project(conn, episode_id)
  lifecycle.rs                 # NEW — apply_decay(conn, space, now, config),
                               #           compact_episodes(conn, space, config)
  communities.rs               # NEW — rebuild_communities(conn, space):
                               #           load adjacency → index::label_propagation →
                               #           upsert communities table
  schema.rs                    # EXTEND — bump LEDGER_SCHEMA_VERSION to 3
  migrations/
    v3.sql                     # NEW — FTS5 virtual table, tfidf_vectors table,
                               #           salience columns, compaction columns
  reproject.rs                 # EXTEND — rebuild indexes + salience + communities
                               #           after replaying ledger

oxibrain/src/
  lib.rs                       # EXTEND — query, traverse, timeline, diff, why,
  #                             why_dropped, assemble_context, apply_decay, compact

benches/
  budget.rs                    # NEW — criterion bench suite (§14)
```

### 4.4 New workspace dependencies

```toml
# Added to [workspace.dependencies]:
criterion = { version = "0.5", features = ["html_reports"] }
```

`oxibrain-index` crate manifest:

```toml
[package]
name = "oxibrain-index"
edition.workspace = true

[dependencies]
oxibrain-core.workspace = true
oxibrain-ports.workspace = true
serde.workspace = true
```

`oxibrain-store` gains `oxibrain-index` as a dependency. The facade
(`oxibrain`) re-exports retrieval types from core. `benches/` is a workspace
member with `criterion` + `oxibrain` + `oxibrain-store` dev-deps.

---

## 5. Data types

### 5.1 Retrieval types (core/retrieval.rs)

```rust
use crate::knowledge::{EntityId, StatementId};
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

/// How a query selects and fuses result modes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub mode: QueryMode,
    pub space: String,
    pub as_of: Option<Timestamp>,    // valid-time filter on beliefs
    pub limit: usize,                // default 20
    pub min_confidence: f32,         // default 0.0 (M2: always passes — confidence is 1.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    Hybrid,    // default — RRF fusion of lexical + semantic + graph
    Lexical,   // FTS5/BM25 only
    Semantic,  // TF-IDF kNN only
    Graph,     // adjacency/traversal only
    Community, // thematic — map-reduce over community clusters
}

/// A raw hit from one retrieval mode, before fusion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub target: SearchTarget,
    pub score: f64,         // mode-specific raw score (BM25, cosine, graph distance)
    pub mode: QueryMode,
}

/// What a hit points at.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchTarget {
    Episode { id: String },
    Statement { id: StatementId },
    Entity { id: EntityId },
}

/// A fused, ranked result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedItem {
    pub target: SearchTarget,
    pub fused_score: f64,          // RRF score
    pub rank: usize,               // 0-indexed position in fused list
    pub mode_ranks: Vec<(QueryMode, usize)>, // rank in each contributing list
    pub salience: f64,
}

/// The complete result of a hybrid query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingResult {
    pub items: Vec<RankedItem>,
    pub dropped: Vec<DroppedItem>,   // §13.3 — instrument what was discarded
    pub total_found: usize,
    pub query: Query,
}

/// Something that was filtered out, with the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedItem {
    pub target: SearchTarget,
    pub reason: DropReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    BelowConfidenceFloor { actual: f32, floor: f32 },
    OutsideValidWindow { valid_at: Timestamp },
    TrustExcluded { tier: String },
    TruncatedByBudget { position: usize },
    BelowSalienceFloor { salience: f64, floor: f64 },
}
```

### 5.2 Traversal types (core/retrieval.rs, cont.)

Per DESIGN §9.3:

```rust
/// Bounded subgraph traversal specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalSpec {
    pub start: Vec<EntityId>,
    pub max_depth: u8,            // hard cap 5
    pub max_nodes: u32,           // hard cap, default 256
    pub predicates: PredicateFilter,
    pub direction: Direction,
    pub valid_at: Option<Timestamp>,
    pub min_confidence: f32,
    pub strategy: Strategy,
}

impl Default for TraversalSpec {
    fn default() -> Self {
        Self {
            start: Vec::new(),
            max_depth: 3,
            max_nodes: 256,
            predicates: PredicateFilter::AllowAll,
            direction: Direction::Both,
            valid_at: None,
            min_confidence: 0.0,
            strategy: Strategy::Bfs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateFilter {
    AllowAll,
    Allow(Vec<String>),
    Deny(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction { Out, In, Both }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Strategy {
    Bfs,
    ShortestPath { to: EntityId },
}

/// A traversal result — the bounded subgraph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalResult {
    pub nodes: Vec<TraversalNode>,
    pub edges: Vec<TraversalEdge>,
    pub truncated: bool,        // true if max_nodes or max_depth was hit
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalNode {
    pub entity: EntityId,
    pub depth: u8,
    pub salience: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalEdge {
    pub from: EntityId,
    pub to: EntityId,
    pub predicate: String,
    pub statement_id: StatementId,
    pub depth: u8,
}
```

### 5.3 Lifecycle types (core/lifecycle.rs)

```rust
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

/// Configuration for time-based salience decay. Deterministic: same inputs → same output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    pub base: f64,           // starting salience (1.0)
    pub lambda: f64,         // decay rate per day (e.g., 0.01 = ~1%/day)
    pub floor: f64,          // salience never drops below this (e.g., 0.05)
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self { base: 1.0, lambda: 0.01, floor: 0.05 }
    }
}

/// Pure salience computation. Deterministic from the ledger.
/// `last_activity` = most recent assertion.recorded_at for the entity.
/// `now` = the evaluation time (passed explicitly for deterministic replay).
pub fn salience(last_activity: Timestamp, now: Timestamp, config: &DecayConfig) -> f64 {
    let age_seconds = (now.as_i64() - last_activity.as_i64()).max(0) as f64;
    let age_days = age_seconds / 86_400.0;
    let decayed = config.base * (-config.lambda * age_days).exp();
    decayed.max(config.floor)
}

/// Configuration for compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub salience_threshold: f64,  // compact episodes whose entities are below this
    pub min_age_days: u32,        // don't compact episodes younger than this
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { salience_threshold: 0.1, min_age_days: 90 }
    }
}

/// A snapshot of an entity's salience, for batch recalculation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalienceEntry {
    pub entity_id: String,
    pub salience: f64,
    pub last_activity: Timestamp,
}
```

### 5.4 Context types (core/context.rs)

```rust
use serde::{Deserialize, Serialize};

/// A token budget for context assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_tokens: usize,
}

/// One packed layer of assembled context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextLayer {
    pub kind: LayerKind,
    pub text: String,
    pub estimated_tokens: usize,
    pub provenance: Vec<String>,   // episode/statement ids
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    PinnedFacts,
    HighSalienceBeliefs,
    QueryNeighborhood,
    RecentEpisodes,
}

/// The assembled context for a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResult {
    pub layers: Vec<ContextLayer>,
    pub total_tokens: usize,
    pub budget: ContextBudget,
    pub truncated: bool,
}

/// Estimate token count from text length. Deterministic: chars / 4.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}
```

### 5.5 Index-internal types (index/)

```rust
// index/rrf.rs
#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: String,       // episode/statement/entity id
    pub score: f64,       // fused RRF score
}

/// Fuse multiple ranked lists with Reciprocal Rank Fusion (Cormack et al. 2009).
/// k = 60 (standard). Pure, deterministic.
pub fn fuse(lists: &[Vec<(String, f64)>], k: u32) -> Vec<RankedItem>;

// index/vector.rs
/// A fixed-dimension TF-IDF vector (hashing trick, D=1024).
pub struct TfIdfVector(Vec<f32>);

pub struct TfIdfModel {
    pub dim: usize,             // 1024
    pub idf: Vec<f32>,          // inverse document frequency per dimension
    pub n_docs: usize,
}

impl TfIdfModel {
    pub fn fit(docs: &[Vec<String>], dim: usize) -> Self;  // build IDF
    pub fn transform(&self, tokens: &[String]) -> TfIdfVector;
    pub fn transform_query(&self, text: &str) -> TfIdfVector;
}

pub fn cosine_sim(a: &TfIdfVector, b: &TfIdfVector) -> f64;
pub fn tokenize(text: &str) -> Vec<String>;  // lowercase, split on non-alnum, stop-word filter

// index/knn.rs
/// In-memory kNN index over TF-IDF vectors. Brute-force cosine for M2 (deterministic).
pub struct KnnIndex {
    entries: Vec<(String, TfIdfVector)>,  // (id, vector)
}

impl KnnIndex {
    pub fn new() -> Self;
    pub fn insert(&mut self, id: String, vector: TfIdfVector);
    pub fn search(&self, query: &TfIdfVector, k: usize) -> Vec<(String, f64)>;
    pub fn len(&self) -> usize;
}

// index/adjacency.rs
pub struct AdjacencyGraph {
    nodes: std::collections::BTreeSet<String>,
    /// outgoing: entity → [(neighbor, predicate, statement_id)]
    outgoing: std::collections::BTreeMap<String, Vec<(String, String, String)>>,
    /// incoming: entity → [(neighbor, predicate, statement_id)]
    incoming: std::collections::BTreeMap<String, Vec<(String, String, String)>>,
}

impl AdjacencyGraph {
    pub fn new() -> Self;
    pub fn add_edge(&mut self, from: &str, to: &str, predicate: &str, stmt: &str);
    pub fn neighbors_out(&self, entity: &str) -> &[(String, String, String)];
    pub fn neighbors_in(&self, entity: &str) -> &[(String, String, String)];
    pub fn bfs(&self, spec: &BfsSpec) -> BfsResult;
}

pub struct BfsSpec {
    pub start: Vec<String>,
    pub max_depth: u8,
    pub max_nodes: u32,
    pub direction: Direction,
    pub predicate_filter: PredicateFilter,
}

pub struct BfsResult {
    pub nodes: std::collections::BTreeMap<String, u8>,  // entity → depth
    pub edges: Vec<(String, String, String, String, u8)>, // from, to, pred, stmt, depth
    pub truncated: bool,
}

// index/community.rs
/// Deterministic label propagation.
/// Tie-break: lowest entity id wins (lexicographic). Fixed iteration cap.
pub fn label_propagation(
    graph: &AdjacencyGraph,
    max_iterations: usize,   // e.g., 10
) -> CommunityMap;

pub struct CommunityMap {
    /// entity_id → community label
    pub labels: std::collections::BTreeMap<String, u64>,
}
```

---

## 6. Retrieval design

### 6.1 Lexical (FTS5/BM25)

Store creates an FTS5 virtual table indexing episode content and statement
renderings:

```sql
-- v3.sql
CREATE VIRTUAL TABLE IF NOT EXISTS episodes_fts USING fts5(
    space_id UNINDEXED,
    target_kind,        -- 'episode' | 'statement'
    target_id,          -- the episode or statement id
    body,               -- text content
    tokenize = 'porter unicode61'
);
```

**Statement renderings:** for each statement, store generates
`{subject_surface} {predicate} {object_repr}` and inserts it as a `statement`
row. This makes the knowledge graph text-searchable. The rendering is derived
from the projection and rebuilt by reprojection.

**FTS5 BM25:** SQLite's `bm25(episodes_fts)` function returns a relevance score
(lower = better in FTS5; we negate it so higher = better). Query:

```sql
SELECT target_kind, target_id, -rank AS score
FROM episodes_fts
WHERE episodes_fts MATCH ?1 AND space_id = ?2
ORDER BY rank
LIMIT ?3;
```

`?1` is the FTS5 query expression (store escapes/transforms the user query into
FTS5 syntax: space-separated terms become implicit AND).

### 6.2 Semantic (TF-IDF kNN)

**TF-IDF model:** built from the corpus of all episode texts + statement
renderings in a space. Uses the hashing trick for fixed dimensionality:

- `dim = 1024` (configurable, default 1024)
- Token → dimension: `fnv1a(token) % dim`
- IDF computed from document frequencies per dimension
- TF-IDF vector: `tf(token) * idf(dimension)` accumulated per dimension
- L2-normalized

The model is rebuilt deterministically by reprojection: same corpus → same IDF
→ same vectors. The model itself is not persisted (rebuilt from the corpus on
each index rebuild); the per-document vectors ARE persisted as BLOBs in
`tfidf_vectors` for fast kNN loading without recomputation.

```sql
-- v3.sql
CREATE TABLE IF NOT EXISTS tfidf_vectors (
    space_id   TEXT NOT NULL,
    target_kind TEXT NOT NULL,     -- 'episode' | 'statement'
    target_id  TEXT NOT NULL,
    vector     BLOB NOT NULL,      -- serialized Vec<f32> (little-endian)
    PRIMARY KEY (space_id, target_kind, target_id)
);
```

**kNN search:** on query, store computes the query TF-IDF vector (using the
in-memory `TfIdfModel`), then searches the in-memory `KnnIndex` (loaded from
`tfidf_vectors` on cold start). Brute-force cosine similarity, top-k. For M2
scale (hand-built graphs, bench fixtures ≤ 10⁴ documents), brute-force is
within the 80ms budget. The `KnnIndex` interface allows swapping in HNSW in M3.

### 6.3 Graph (adjacency traversal)

Adjacency is a view over `statements` where `object_entity IS NOT NULL` (entity
to entity edges). The `AdjacencyGraph` is loaded from the statements table on
cold start and rebuilt by reprojection.

**Belief filter:** only edges backed by at least one `Active` or `Superseded`
belief at `valid_at` (if specified) are traversable. `Contradicted` and
`Retracted` edges are excluded from traversal (but still queryable via `why`).

**Traversal execution (store):** load `AdjacencyGraph` from statements → call
`graph.bfs(spec)` → enrich nodes with salience → return `TraversalResult`.

### 6.4 Hybrid (RRF fusion)

RRF fuses the ranked lists from lexical, semantic, and graph modes:

```
score(d) = Σ_{i} 1 / (k + rank_i(d))     where k = 60
```

Only modes that produce results contribute. Each mode returns at most `limit`
hits. RRF re-ranks across modes. Items appearing in multiple modes get higher
fused scores (the point of fusion).

Store calls `index::rrf::fuse(&[lexical_list, semantic_list, graph_list], 60)`
and gets back fused `RankedItem`s. Then enriches with salience, beliefs, and
provenance.

### 6.5 Community (thematic)

**Label propagation** (§9.4): clusters the entity graph into communities.
Deterministic:

1. Initialize each entity's label to its ordinal position (sorted by entity id).
2. For `max_iterations` rounds (default 10):
   - Process entities in sorted order (by entity id — deterministic).
   - Each entity adopts the most frequent label among its neighbors.
   - Ties broken by lowest label value (deterministic).
3. Convergence is NOT required — the fixed cap guarantees termination.

The resulting `CommunityMap` (entity → label) is stored in the `communities`
table (already exists in v1.sql). Reprojection rebuilds it deterministically.

**Community query:** given a query, find the communities of the seed entities
(from lexical/semantic hits), then return all entities in those communities,
ranked by salience. This answers "what have I been working on?" (thematic).

Summary **text** is M3 (LLM-generated, cached). M2 returns the cluster members
with their beliefs — enough for thematic exploration.

---

## 7. Context assembly (`assemble_context`)

The tiered-memory packing policy (§9.5). `assemble_context(query, budget)`
returns a `ContextResult` packing layers to the token budget:

| Layer | Source | Priority |
|---|---|---|
| Pinned facts | explicitly pinned entities/beliefs (M2: none pinned yet — pin API is M4) | 1 (highest) |
| High-salience beliefs | top-N beliefs by salience for query-relevant entities | 2 |
| Query neighborhood | 1-hop adjacency of query seed entities | 3 |
| Recent episodes | most recent N episodes (by `ingested_at`) | 4 |

**Packing algorithm (deterministic):**
1. Compute token estimate for each candidate layer item.
2. Pack in priority order until budget is exhausted.
3. Within each layer, rank by salience (descending), then by recency.
4. If the budget is exhausted mid-layer, set `truncated = true`.
5. Attach provenance (episode/statement ids) to each layer.

Each belief is rendered as: `{subject} {predicate} {object} (since {valid_from},
confidence {confidence}, salience {salience:.2})`. Episodes are rendered as
their content text (truncated to fit).

**This is reconstruction, not retrieval** (§9.5). The context is composed on
demand from beliefs, neighborhoods, and episodes — not a stored blob.

---

## 8. Lifecycle

### 8.1 Salience decay

**Model:** time-based exponential decay from the entity's last assertion
timestamp. Pure function (§5.3). Deterministic from the ledger.

**Cached column:** `entities.salience` (REAL) and `entities.last_activity`
(INTEGER, = max(assertions.recorded_at) for the entity's statements). Updated
on every projection write (when new assertions arrive) and by the periodic
decay WriteOp.

**Decay WriteOp (`apply_decay`):** recalculates `salience` for all entities in
a space using the pure `salience()` fn with the current time. This is a batch
update on the writer actor. It does NOT write to the ledger — it only updates
the projection cache. Reprojection rebuilds it identically (same formula, same
timestamps).

**Why this is deterministic:** `salience = f(last_activity, now, config)`.
`last_activity` is `max(recorded_at)` from assertions (ledger-derived). `now`
is passed explicitly (the `ClockPort` value at decay time). `config` is
constant. Same ledger + same `now` → same salience. Reprojection passes each
episode's `ingested_at` as `now` (same pattern as M1 projection), so the
cached salience after reprojection matches the incremental cache.

Wait — that's wrong. Salience depends on the *current* time, not the ingest
time. After reprojection, what time do we use? The answer: **reprojection does
not recompute salience at all** — salience is a query-time concern. The cached
`entities.salience` column is a performance optimization that is rebuilt
lazily (by `apply_decay`) after reprojection, using the wall-clock `now`. It
is NOT part of the byte-identical reprojection test.

**Resolution:** The `entities.salience` and `entities.last_activity` columns
are projection-cache columns that are:
- Populated during normal projection writes (incremental update).
- Recalculated by `apply_decay` (periodic batch).
- Left NULL (or set to a default) by reprojection, then recalculated by a
  post-reproject `apply_decay` call.
- Excluded from the byte-identical reprojection test scope (like indexes —
  they are derived caches, not core projection state).

This is consistent with P1: salience is derived from the ledger (via
`last_activity`), and the projection (beliefs, statements, entities) is
byte-identical. The salience cache is a performance layer on top, rebuilt from
the same deterministic inputs.

### 8.2 Compaction

Moves cold episode content to a compressed BLOB column:

```sql
-- v3.sql (adds columns to existing episodes table)
ALTER TABLE episodes ADD COLUMN content_compacted BLOB;
ALTER TABLE episodes ADD COLUMN compacted_at INTEGER;
```

**Compaction WriteOp (`compact_episodes`):**
1. Find episodes where all associated entities have salience below
   `salience_threshold` AND age > `min_age_days`.
2. For each: compress `content` with flate2 (zlib), store in
   `content_compacted`, set `compacted_at = now`, set `content = ""` (empty,
   not NULL — the row and hash are preserved).
3. This is a projection-cache optimization. Reprojection does NOT compact —
   it replays full content. Compaction is re-applied by a post-reproject call.

**Reading compacted episodes:** `get_episode` checks `content_compacted` first;
if non-null, decompresses. The `content` field is always available to callers,
transparently decompressed if needed.

**Provenance integrity:** the episode row, id, hash, and all links remain.
Compaction never breaks provenance (§10). Only the content bytes are compressed.

---

## 9. Explainability queries

### 9.1 `timeline(entity, from, to)`

Returns belief intervals for an entity over `[from, to]`:

```rust
pub struct TimelineEntry {
    pub statement_id: StatementId,
    pub predicate: String,
    pub object_repr: String,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub status: String,       // Active, Superseded, Contradicted, Retracted
    pub recorded_at: Timestamp,
}
```

Query: join beliefs → statements for the entity's merge group, filtered by
`valid_from <= to AND valid_to >= from`, ordered by `valid_from`.

### 9.2 `diff(entity, at_a, at_b)`

What changed between two time points:

```rust
pub struct DiffResult {
    pub added: Vec<TimelineEntry>,       // beliefs active at at_b but not at_a
    pub removed: Vec<TimelineEntry>,     // beliefs active at at_a but not at_b
    pub changed: Vec<TimelineEntry>,     // same statement, different interval/status
}
```

Computed by calling `beliefs_as_of` at `at_a` and `at_b`, then diffing.

### 9.3 `why(statement_id)`

Provenance and confidence breakdown (§9.6):

```rust
pub struct ExplainBlock {
    pub statement: Statement,
    pub beliefs: Vec<Belief>,
    pub assertions: Vec<AssertionDetail>,
    pub episodes: Vec<EpisodeRef>,
    pub confidence_breakdown: ConfidenceBreakdown,
}

pub struct AssertionDetail {
    pub assertion: Assertion,
    pub episode_id: String,
    pub extractor: Option<String>,
    pub mention_text: Option<String>,
}

pub struct ConfidenceBreakdown {
    pub raw_confidence: f32,         // from fold (1.0 in M2)
    pub calibrated: Option<f32>,     // None in M2 (calibration is M3)
    pub support_count: usize,
    pub contradiction_count: usize,
}
```

### 9.4 `why_dropped(query)`

What was filtered during a query and why (§13.3). Returns the `dropped` field
of `RankingResult` — items that were found but excluded, with `DropReason`.
This makes the control plane's filtering decisions visible.

---

## 10. Determinism (P1)

### 10.1 Index rebuild determinism

All indexes are deterministic functions of the projection:

| Index | Input | Deterministic because |
|---|---|---|
| FTS5 | episode content + statement renderings | content is ledger-derived; renderings use canonical forms |
| TF-IDF model | corpus of texts | hashing trick is deterministic (FNV1a hash); IDF is a pure aggregate |
| TF-IDF vectors | model + per-doc tokens | tokenize is deterministic (lowercase, split, stop-words) |
| KNN index | vectors | brute-force cosine — no randomness |
| Adjacency | statements | entity ids are content-derived; edge order is insertion order (by seq) |
| Communities | adjacency | label propagation with fixed cap + sorted processing + id tie-break |

### 10.2 Reprojection + index rebuild

`reproject()` now:
1. Drops projection tables (beliefs, statements, assertions, etc.) — M1 behavior.
2. Drops index tables (episodes_fts content, tfidf_vectors, communities) — NEW.
3. Replays the ledger in canonical (seq) order — M1 behavior.
4. Rebuilds indexes: FTS5 re-index, TF-IDF model fit + vector recompute, KNN
   reload, adjacency rebuild, community label propagation — NEW.
5. Recalculates `last_activity` per entity (max recorded_at) — NEW.

Salience (`entities.salience`) is left at default (1.0) after reprojection and
recalculated by a post-reproject `apply_decay(now)` call. It is excluded from
the byte-identical test because it depends on wall-clock `now`, not the ledger.

### 10.3 Byte-identical test scope

The `reproject_is_byte_identical` test (M1) compares the **core projection**
(entities, entity_keys, entity_merges, statements, assertions, mentions,
beliefs, predicates). M2 extends this to also compare:
- `episodes_fts` content (target_kind, target_id, body — not the internal FTS
  b-tree, which is an implementation detail)
- `tfidf_vectors` (target_id, vector BLOB)
- `communities` (entity_id → label)

Salience and compaction columns are excluded (they are time-dependent caches).

---

## 11. Testing strategy

### 11.1 Property tests (core + index)

- **RRF fusion:** fusing identical lists yields the input order; fusing
  disjoint lists interleaves by rank; fusion is invariant to list ordering
  (when lists are sorted by score).
- **TF-IDF:** identical documents produce identical vectors; cosine sim of a
  document with itself is 1.0 (± epsilon); tokenize is idempotent
  (tokenize(tokenize(x)) == tokenize(x)).
- **KNN:** search returns at most k items; the query vector itself is the
  nearest neighbor if inserted; brute-force results match a reference sort.
- **Label propagation:** deterministic across runs (same graph → same
  communities); isolated nodes form singleton communities; connected components
  converge to one label within the iteration cap.

### 11.2 Integration tests (store + facade)

- **Hand-built graph traversal:** declare a multi-hop graph (A→B→C→D), traverse
  from A at depth 3, assert the subgraph. Assert bounds (max_nodes truncates).
- **Hybrid query over hand-built graph:** declare entities + statements, ingest
  episodes with text, run hybrid query, assert results contain expected hits
  fused across modes.
- **Community query:** declare a graph with two natural clusters, assert
  label-propagation separates them.
- **`timeline` / `diff` / `why`:** over a supersession scenario (M1 graph),
  assert correct intervals, diff, and provenance.
- **`assemble_context`:** assert packing respects token budget, layers are in
  priority order, provenance is attached.
- **`why --dropped`:** assert dropped items have correct reasons.
- **Decay:** apply_decay, assert salience decreased monotonically and respects
  floor.
- **Compaction:** compact episodes, assert content is retrievable (decompressed),
  hash unchanged.

### 11.3 Determinism tests

- **Index rebuild determinism:** build indexes, snapshot (FTS content, vectors,
  communities), reproject, snapshot again, assert byte-identical.
- **Extended `reproject_is_byte_identical`:** M1 test extended to include index
  tables in the comparison.

### 11.4 Bench suite (§14)

See §14.

---

## 12. Schema changes (v3 migration)

### 12.1 New tables and columns

```sql
-- v3.sql

-- FTS5 virtual table for lexical search
CREATE VIRTUAL TABLE IF NOT EXISTS episodes_fts USING fts5(
    space_id UNINDEXED,
    target_kind,
    target_id,
    body,
    tokenize = 'porter unicode61'
);

-- TF-IDF vector storage (BLOB-serialized Vec<f32>)
CREATE TABLE IF NOT EXISTS tfidf_vectors (
    space_id    TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    vector      BLOB NOT NULL,
    PRIMARY KEY (space_id, target_kind, target_id)
);

-- Salience cache columns on entities
ALTER TABLE entities ADD COLUMN salience REAL NOT NULL DEFAULT 1.0;
ALTER TABLE entities ADD COLUMN last_activity INTEGER;

-- Compaction columns on episodes
ALTER TABLE episodes ADD COLUMN content_compacted BLOB;
ALTER TABLE episodes ADD COLUMN compacted_at INTEGER;
```

The `communities` table already exists (v1.sql) with columns `(id, space_id,
label)`. M2 populates it: `id` = entity_id, `space_id` = space, `label` =
community label (u64).

### 12.2 Version bump

`schema::LEDGER_SCHEMA_VERSION` bumps from `2` to `3`. The migration chain test
extends to cover v2→v3.

### 12.3 Post-migration index build

After applying v3.sql, the migration step calls `rebuild_indexes` for all
spaces (populates FTS5, tfidf_vectors from existing projection data). This is
a data migration, not a schema change.

---

## 13. Facade API (oxibrain/src/lib.rs)

```rust
impl Brain {
    // --- Retrieval ---

    /// Hybrid (or mode-specific) query. Returns ranked results with provenance.
    pub async fn query(&self, q: Query) -> Result<RankingResult, BrainError>;

    /// Bounded subgraph traversal.
    pub async fn traverse(&self, spec: TraversalSpec) -> Result<TraversalResult, BrainError>;

    // --- Explainability ---

    /// Belief intervals for an entity over [from, to].
    pub async fn timeline(
        &self, space: &str, entity_id: &str,
        from: Option<Timestamp>, to: Option<Timestamp>,
    ) -> Result<Vec<TimelineEntry>, BrainError>;

    /// What changed for an entity between two time points.
    pub async fn diff(
        &self, space: &str, entity_id: &str,
        at_a: Timestamp, at_b: Timestamp,
    ) -> Result<DiffResult, BrainError>;

    /// Provenance and confidence breakdown for a statement.
    pub async fn why(&self, space: &str, statement_id: &str) -> Result<ExplainBlock, BrainError>;

    // --- Context assembly ---

    /// Pack context for a query to a token budget.
    pub async fn assemble_context(
        &self, query: &str, token_budget: usize,
    ) -> Result<ContextResult, BrainError>;

    // --- Lifecycle ---

    /// Recalculate salience for all entities (decay).
    pub async fn apply_decay(&self, space: &str) -> Result<usize, BrainError>;

    /// Compact cold episodes (compress content).
    pub async fn compact(&self, space: &str) -> Result<usize, BrainError>;
}
```

All methods follow the M1 pattern: writes (decay, compact) use
`mpsc + writer.flush() + spawn_blocking`; reads use `spawn_blocking +
readers.read`. Space is passed as the **content-derived ID** (from
`ensure_space`), not the name — consistent with the handoff §2.2 finding.

---

## 14. Bench suite

### 14.1 Structure

`benches/budget.rs` uses criterion. Each bench measures one §13.2 budget
against a deterministic synthetic fixture:

| Bench | Measures | Budget |
|---|---|---|
| `declaration_write` | `project_declaration` for one statement | < 5 ms |
| `get_entity` | `beliefs_for_entity` (with merge chain) | < 10 ms |
| `hybrid_query_top20` | `hybrid_query` limit 20 | < 80 ms |
| `traversal_depth3_256` | `traverse` depth 3, max_nodes 256 | < 100 ms |
| `assemble_context_3k` | `assemble_context` 3000 tokens | < 150 ms |
| `reproject_from_cache` | full `reproject` (fixture-scale) | < 5 min |
| `cold_start_index_load` | open store + load indexes into memory | < 2 s |

### 14.2 Fixture

A deterministic synthetic fixture generated in the bench setup:
- 1 space, 200 entities, 500 statements, 1000 assertions, 200 episodes.
- Small enough to run in CI (< 30s total), large enough to exercise the indexes.
- Generated by a deterministic builder (fixed seed — no randomness).

### 14.3 Budget revision protocol

Each budget may be revised **once** with measurement + reason recorded in
DESIGN.md §13.2 (D16). After revision, it becomes a regression gate. The bench
suite's criterion output records the numbers; a companion test asserts
non-regression against the committed baseline.

---

## 15. Deviations from DESIGN.md

| # | Deviation | DESIGN says | M2 does | Reason |
|---|---|---|---|---|
| D1 | sqlite-vec deferred | §9.1: "sqlite-vec persisted" | TF-IDF vectors stored as BLOBs in a regular table | sqlite-vec requires extension loading incompatible with bundled rusqlite; TF-IDF is the M2 baseline. sqlite-vec arrives in M3 with dense embeddings. |
| D2 | HNSW deferred | §9.1: "HNSW in memory" | Brute-force cosine kNN (deterministic) | HNSW uses random levels (non-deterministic). Brute-force is deterministic and adequate at M2 scale. HNSW with hash-based levels arrives in M3. |
| D3 | Core stays pure | §15: "core may depend on store, index" | Core defines types + pure formulas only; store orchestrates retrieval | Consistent with M1. Core gaining store deps is M5+. Avoids circular deps. |
| D4 | Salience is time-decay only | §9.2: "PageRank/co-access, decay, access frequency" | Time-decay from assertion timestamps (deterministic) | Access frequency requires persisting query logs (M4+). PageRank over the graph is a salience signal but adds complexity. Time-decay is deterministic from the ledger. |
| D5 | Salience excluded from byte-identical test | P1: "projection is byte-identical" | Salience cache depends on wall-clock `now`; excluded from reprojection comparison | Salience is derived from the ledger (via `last_activity`) but the cached value depends on `now`. Core projection (beliefs, entities, statements) remains byte-identical. |
| D6 | Pinned facts layer empty | §9.5: "pinned facts" | No pin API in M2 (pinning is M4) | The layer exists in the packing policy but has no items until the pin API arrives. |
| D7 | Community summary text deferred | §9.4: "summarize each cluster" | Clustering only; summary text is M3 | Summary text is LLM-generated (cached `Derived` episode). M2 has no LLM. Clustering is deterministic and useful without text. |

---

## 16. Open questions (M2 defaults)

1. **FTS5 tokenizer.** `porter unicode61` handles English + Unicode. For CJK
   text (no word boundaries), bigram tokenization may be needed. *Default:
   `porter unicode61` for M2; revisit with real multilingual corpora in M3.*

2. **TF-IDF dimensionality.** D=1024 via hashing trick balances collision rate
   and memory. *Default: 1024; revisit if eval shows collisions hurt recall.*

3. **Community iteration cap.** 10 iterations is enough for small graphs. For
   10⁵ entities, convergence may need more. *Default: 10; revisit with scale
   data. The cap guarantees termination regardless.*

4. **Compaction compression level.** flate2 default (level 6). *Default: level
   6; the bench suite will measure if it's a bottleneck.*

5. **Statement rendering format.** `{subject} {predicate} {object}` is simple
   but loses entity type context. *Default: simple format; revisit if FTS
   recall is poor on entity-type queries.*

6. **Query vector from text vs. entity.** The semantic mode computes a TF-IDF
   vector from the query *text*. An alternative is to use the seed entities'
   averaged vectors. *Default: query text vector; entity-averaged is a future
   enhancement if text-only proves insufficient.*

---

End of spec. Read this + `doc/DESIGN.md` §9 (retrieval), §10 (lifecycle),
§13.2 (budgets), §17 (M2) + the M1→M2 handoff
(`docs/superpowers/handoffs/2026-08-11-m1-to-m2.md`) — then proceed to the
implementation plan.
