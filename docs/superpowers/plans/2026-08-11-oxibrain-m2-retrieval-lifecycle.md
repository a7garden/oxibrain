# oxibrain M2 — Retrieval & Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan
> task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the retrieval and lifecycle layer — FTS5/BM25 lexical search,
TF-IDF semantic kNN, adjacency traversal, label-propagation communities, RRF
hybrid fusion, salience decay, compaction, context assembly, explainability
queries, and the bench suite — all deterministic, no LLM.

**Architecture:** New `oxibrain-index` crate (pure algorithms, depends on core
+ ports) sits between core and store. Store orchestrates all SQLite/FTS5
execution and calls index algorithms. Core defines retrieval/lifecycle/context
types. Facade adds async methods following the M1 mpsc+spawn_blocking pattern.

**Tech Stack:** Rust 2024, rusqlite (FTS5), criterion (benches), serde.

## Global Constraints

- Rust 2024 edition, MSRV 1.85.
- `clippy --all-targets --all-features -- -D warnings` clean.
- `#![cfg_attr(test, allow(clippy::unwrap_used))]` in every crate root.
- Timestamp API: `Timestamp::from_millis(i64)` / `Timestamp::millis() -> i64`.
  NEVER use `.as_i64()`.
- rusqlite errors → `crate::sql_err(e)?` (the store-local helper). NEVER `?` on
  rusqlite directly (orphan rule blocks auto-conversion).
- Only `oxibrain-store` may reference `rusqlite`. Index and core are pure.
- Content-derived ids; no randomness in anything persisted.
- Space is passed as the content-derived ID (from `ensure_space`), not the name.
- Comments and commit messages in English.

---

## File Structure

```
crates/
├── oxibrain-index/              # NEW crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs               # re-exports
│       ├── rrf.rs               # RRF fusion (pure)
│       ├── vector.rs            # TF-IDF model + tokenizer (pure)
│       ├── knn.rs               # KnnIndex brute-force cosine (pure)
│       ├── adjacency.rs         # AdjacencyGraph + BFS (pure)
│       └── community.rs         # label propagation (pure)
├── oxibrain-core/src/
│   ├── retrieval.rs             # NEW — Query, TraversalSpec, RankingResult types
│   ├── lifecycle.rs             # NEW — DecayConfig, salience, CompactionConfig
│   └── context.rs               # NEW — ContextBudget, ContextResult, estimate_tokens
├── oxibrain-store/src/
│   ├── query.rs                 # EXTEND — hybrid_query, fts_search, tfidf_knn, traverse
│   ├── timeline.rs              # NEW — timeline, diff
│   ├── explain.rs               # NEW — why, why_dropped
│   ├── index_ops.rs             # NEW — rebuild_indexes, update_on_project
│   ├── lifecycle.rs             # NEW — apply_decay, compact_episodes
│   ├── communities.rs           # NEW — rebuild_communities, community_query
│   ├── schema.rs                # EXTEND — LEDGER_SCHEMA_VERSION = 3
│   ├── migrations/v3.sql        # NEW
│   ├── migration.rs             # EXTEND — v3 step
│   └── reproject.rs             # EXTEND — rebuild indexes after replay
├── oxibrain/src/
│   └── lib.rs                   # EXTEND — query, traverse, timeline, diff, why, etc.
└── oxibrain/benches/
    └── budget.rs                # NEW — criterion bench suite
```

---

## Task 1: Scaffold oxibrain-index crate + core types

**Files:**
- Create: `crates/oxibrain-index/Cargo.toml`
- Create: `crates/oxibrain-index/src/lib.rs`
- Create: `crates/oxibrain-core/src/retrieval.rs`
- Create: `crates/oxibrain-core/src/lifecycle.rs`
- Create: `crates/oxibrain-core/src/context.rs`
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/oxibrain-core/src/lib.rs`
- Modify: `crates/oxibrain-store/Cargo.toml`

**Interfaces:**
- Produces: `oxibrain_index` crate (empty lib re-exporting nothing yet); core
  types: `Query`, `QueryMode`, `TraversalSpec`, `RankingResult`, `DecayConfig`,
  `salience()`, `ContextBudget`, `estimate_tokens`.

- [ ] **Step 1: Add workspace member and deps**

Modify `Cargo.toml` (workspace root):

```toml
[workspace]
resolver = "2"
members = [
    "crates/oxibrain-ports",
    "crates/oxibrain-core",
    "crates/oxibrain-index",
    "crates/oxibrain-store",
    "crates/oxibrain",
    "crates/oxibrain-cli",
]

[workspace.dependencies]
oxibrain = { path = "crates/oxibrain", version = "0.1.0" }
oxibrain-core = { path = "crates/oxibrain-core", version = "0.1.0" }
oxibrain-index = { path = "crates/oxibrain-index", version = "0.1.0" }
oxibrain-ports = { path = "crates/oxibrain-ports", version = "0.1.0" }
oxibrain-store = { path = "crates/oxibrain-store", version = "0.1.0" }
blake3 = "1.5"
hex = "0.4"
unicode-normalization = "0.1"
strsim = "0.11"
fs2 = "0.4"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.32", features = ["bundled", "backup"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "fs"] }
clap = { version = "4", features = ["derive"] }
proptest = "1"
tempfile = "3"
tracing = "0.1"
tracing-subscriber = "0.3"
thiserror = "1"
anyhow = "1"
criterion = { version = "0.5", features = ["html_reports"] }
```

- [ ] **Step 2: Create oxibrain-index crate**

`crates/oxibrain-index/Cargo.toml`:
```toml
[package]
name = "oxibrain-index"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
oxibrain-core.workspace = true
oxibrain-ports.workspace = true
serde.workspace = true

[dev-dependencies]
proptest.workspace = true
```

`crates/oxibrain-index/src/lib.rs`:
```rust
//! oxibrain-index: pure retrieval algorithms (RRF, TF-IDF, kNN, adjacency,
//! community label propagation). No rusqlite, no I/O — all algorithms are pure
//! functions over in-memory data structures.

#![cfg_attr(test, allow(clippy::unwrap_used))]
```

- [ ] **Step 3: Create core/retrieval.rs**

`crates/oxibrain-core/src/retrieval.rs`:
```rust
//! Retrieval types: Query, TraversalSpec, RankingResult (DESIGN §9).
//! Type definitions only — execution lives in store.

use crate::knowledge::{EntityId, StatementId};
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub mode: QueryMode,
    pub space: String,
    #[serde(default)]
    pub as_of: Option<Timestamp>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub min_confidence: f32,
}

fn default_limit() -> usize { 20 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    Hybrid,
    Lexical,
    Semantic,
    Graph,
    Community,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub target: SearchTarget,
    pub score: f64,
    pub mode: QueryMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchTarget {
    Episode { id: String },
    Statement { id: StatementId },
    Entity { id: EntityId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedItem {
    pub target: SearchTarget,
    pub fused_score: f64,
    pub rank: usize,
    pub mode_ranks: Vec<(QueryMode, usize)>,
    pub salience: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingResult {
    pub items: Vec<RankedItem>,
    pub dropped: Vec<DroppedItem>,
    pub total_found: usize,
    pub query: Query,
}

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

// --- Traversal ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalSpec {
    pub start: Vec<EntityId>,
    pub max_depth: u8,
    pub max_nodes: u32,
    pub predicates: PredicateFilter,
    pub direction: Direction,
    #[serde(default)]
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

impl PredicateFilter {
    pub fn allows(&self, predicate: &str) -> bool {
        match self {
            PredicateFilter::AllowAll => true,
            PredicateFilter::Allow(list) => list.iter().any(|p| p == predicate),
            PredicateFilter::Deny(list) => !list.iter().any(|p| p == predicate),
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalResult {
    pub nodes: Vec<TraversalNode>,
    pub edges: Vec<TraversalEdge>,
    pub truncated: bool,
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

- [ ] **Step 4: Create core/lifecycle.rs**

`crates/oxibrain-core/src/lifecycle.rs`:
```rust
//! Lifecycle types: salience decay, compaction config (DESIGN §10).

use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    pub base: f64,
    pub lambda: f64,
    pub floor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self { base: 1.0, lambda: 0.01, floor: 0.05 }
    }
}

/// Pure salience computation. Deterministic from the ledger.
pub fn salience(last_activity: Timestamp, now: Timestamp, config: &DecayConfig) -> f64 {
    let age_millis = (now.millis() - last_activity.millis()).max(0) as f64;
    let age_days = age_millis / 86_400_000.0;
    let decayed = config.base * (-config.lambda * age_days).exp();
    decayed.max(config.floor)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub salience_threshold: f64,
    pub min_age_days: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { salience_threshold: 0.1, min_age_days: 90 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalienceEntry {
    pub entity_id: String,
    pub salience: f64,
    pub last_activity: Timestamp,
}
```

- [ ] **Step 5: Create core/context.rs**

`crates/oxibrain-core/src/context.rs`:
```rust
//! Context assembly types (DESIGN §9.5).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBudget {
    pub max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextLayer {
    pub kind: LayerKind,
    pub text: String,
    pub estimated_tokens: usize,
    pub provenance: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    PinnedFacts,
    HighSalienceBeliefs,
    QueryNeighborhood,
    RecentEpisodes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResult {
    pub layers: Vec<ContextLayer>,
    pub total_tokens: usize,
    pub budget: ContextBudget,
    pub truncated: bool,
}

pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}
```

- [ ] **Step 6: Update core/lib.rs re-exports**

Add to `crates/oxibrain-core/src/lib.rs` after the existing `pub mod fold;`:

```rust
pub mod retrieval;
pub mod lifecycle;
pub mod context;

pub use retrieval::{
    Direction, DropReason, DroppedItem, PredicateFilter, Query, QueryMode,
    RankedItem, RankingResult, SearchHit, SearchTarget, Strategy, TraversalEdge,
    TraversalNode, TraversalResult, TraversalSpec,
};
pub use lifecycle::{CompactionConfig, DecayConfig, SalienceEntry, salience};
pub use context::{
    ContextBudget, ContextLayer, ContextResult, LayerKind, estimate_tokens,
};
```

- [ ] **Step 7: Add oxibrain-index dep to store**

Modify `crates/oxibrain-store/Cargo.toml` — add to `[dependencies]`:
```toml
oxibrain-index.workspace = true
```

- [ ] **Step 8: Verify compile**

```bash
cargo build --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(m2): scaffold oxibrain-index crate + retrieval/lifecycle/context types"
```

---

## Task 2: RRF fusion + TF-IDF vector model (index)

**Files:**
- Create: `crates/oxibrain-index/src/rrf.rs`
- Create: `crates/oxibrain-index/src/vector.rs`
- Modify: `crates/oxibrain-index/src/lib.rs`

**Interfaces:**
- Produces: `index::rrf::fuse`, `index::vector::{TfIdfModel, TfIdfVector,
  tokenize, cosine_sim}`.

- [ ] **Step 1: Write rrf.rs**

```rust
//! Reciprocal Rank Fusion (Cormack et al. 2009). Pure, deterministic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusedItem {
    pub key: String,
    pub score: f64,
}

/// Fuse multiple ranked lists. Each list is `(key, raw_score)` pairs in
/// descending score order. `k` is the RRF constant (standard: 60).
/// Returns items sorted by fused score descending.
pub fn fuse(lists: &[Vec<(String, f64)>], k: u32) -> Vec<FusedItem> {
    let mut scores: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for list in lists {
        for (rank, (key, _raw)) in list.iter().enumerate() {
            *scores.entry(key.as_str()).or_default() += 1.0 / (k as f64 + rank as f64 + 1.0);
        }
    }
    let mut items: Vec<FusedItem> = scores
        .into_iter()
        .map(|(key, score)| FusedItem { key: key.to_string(), score })
        .collect();
    // Deterministic sort: score descending, then key ascending (tie-break).
    items.sort_by(|a, b| {
        b.score.partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_list_preserves_order() {
        let list = vec![("a".into(), 1.0), ("b".into(), 0.5)];
        let fused = fuse(&[list.clone()], 60);
        assert_eq!(fused[0].key, "a");
        assert_eq!(fused[1].key, "b");
    }

    #[test]
    fn item_in_multiple_lists_scores_higher() {
        let list1 = vec![("a".into(), 1.0), ("b".into(), 0.5)];
        let list2 = vec![("a".into(), 1.0), ("c".into(), 0.5)];
        let fused = fuse(&[list1, list2], 60);
        assert_eq!(fused[0].key, "a");
        assert!(fused[0].score > fused[1].score);
    }

    #[test]
    fn empty_lists_return_empty() {
        let fused = fuse(&[], 60);
        assert!(fused.is_empty());
    }

    #[test]
    fn tie_break_is_deterministic() {
        let list = vec![("b".into(), 1.0), ("a".into(), 1.0)];
        let fused = fuse(&[list], 60);
        // Same score → alphabetical tie-break
        assert_eq!(fused[0].key, "a");
        assert_eq!(fused[1].key, "b");
    }
}
```

- [ ] **Step 2: Write vector.rs**

```rust
//! TF-IDF vector model with hashing trick (deterministic, fixed dimensionality).

/// FNV-1a hash for dimension assignment. Deterministic.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// A stop-word set for English. Small, deterministic.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "must", "can", "of", "in", "on", "at", "to",
    "for", "with", "by", "from", "as", "into", "about", "than", "then",
    "no", "not", "or", "and", "but", "if", "so", "it", "its", "this",
    "that", "these", "those", "i", "you", "he", "she", "we", "they",
];

/// Tokenize text: lowercase, split on non-alphanumeric, filter stop words.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() > 1 && !STOP_WORDS.contains(s))
        .map(String::from)
        .collect()
}

pub struct TfIdfModel {
    pub dim: usize,
    /// IDF per dimension: ln((1 + N) / (1 + df_d)) + 1  (smooth IDF).
    idf: Vec<f32>,
    pub n_docs: usize,
}

impl TfIdfModel {
    /// Fit the model from raw texts. Builds IDF from document frequencies.
    pub fn fit(texts: &[&str], dim: usize) -> Self {
        let n_docs = texts.len();
        let mut df = vec![0u32; dim];
        for text in texts {
            let tokens = tokenize(text);
            let mut seen = std::collections::HashSet::new();
            for token in &tokens {
                let d = (fnv1a(token) as usize) % dim;
                seen.insert(d);
            }
            for d in seen {
                df[d] += 1;
            }
        }
        let idf = df
            .iter()
            .map(|&d| (((1.0 + n_docs as f32) / (1.0 + d as f32)).ln() + 1.0))
            .collect();
        Self { dim, idf, n_docs }
    }

    /// Transform text into a TF-IDF vector (L2-normalized).
    pub fn transform(&self, text: &str) -> TfIdfVector {
        let tokens = tokenize(text);
        let mut vec = vec![0.0f32; self.dim];
        for token in &tokens {
            let d = (fnv1a(token) as usize) % self.dim;
            vec[d] += 1.0; // term frequency
        }
        // Apply IDF + L2 normalize.
        let mut norm = 0.0f32;
        for (i, v) in vec.iter_mut().enumerate() {
            *v *= self.idf[i];
            norm += *v * *v;
        }
        norm = norm.sqrt().max(1e-12);
        for v in &mut vec {
            *v /= norm;
        }
        TfIdfVector(vec)
    }
}

pub struct TfIdfVector(Vec<f32>);

impl TfIdfVector {
    pub fn as_slice(&self) -> &[f32] { &self.0 }
    pub fn from_vec(v: Vec<f32>) -> Self { Self(v) }
    pub fn dim(&self) -> usize { self.0.len() }

    /// Serialize to bytes (little-endian f32) for persistence.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.0.len() * 4);
        for &v in &self.0 {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let vec: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Self(vec)
    }
}

/// Cosine similarity of two L2-normalized vectors = dot product.
pub fn cosine_sim(a: &TfIdfVector, b: &TfIdfVector) -> f64 {
    let av = a.as_slice();
    let bv = b.as_slice();
    let len = av.len().min(bv.len());
    let mut dot = 0.0f32;
    for i in 0..len {
        dot += av[i] * bv[i];
    }
    dot as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_and_filters() {
        let tokens = tokenize("The Quick Brown Fox");
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"fox".to_string()));
        assert!(!tokens.contains(&"the".to_string())); // stop word
    }

    #[test]
    fn identical_text_identical_vector() {
        let model = TfIdfModel::fit(&["hello world", "foo bar"], 128);
        let v1 = model.transform("hello world");
        let v2 = model.transform("hello world");
        assert_eq!(v1.as_slice(), v2.as_slice());
    }

    #[test]
    fn cosine_self_is_one() {
        let model = TfIdfModel::fit(&["hello world", "foo bar"], 128);
        let v = model.transform("hello world");
        let sim = cosine_sim(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn vector_roundtrip_bytes() {
        let model = TfIdfModel::fit(&["hello world"], 128);
        let v = model.transform("hello world");
        let bytes = v.to_bytes();
        let v2 = TfIdfVector::from_bytes(&bytes);
        assert_eq!(v.as_slice(), v2.as_slice());
    }
}
```


- [ ] **Step 3: Update index/lib.rs**

```rust
pub mod rrf;
pub mod vector;

pub use rrf::{FusedItem, fuse};
pub use vector::{TfIdfModel, TfIdfVector, cosine_sim, tokenize};
```

- [ ] **Step 4: Verify**

```bash
cargo test -p oxibrain-index
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): RRF fusion + TF-IDF vector model in oxibrain-index"
```

---

## Task 3: KNN index + adjacency graph + BFS (index)

**Files:**
- Create: `crates/oxibrain-index/src/knn.rs`
- Create: `crates/oxibrain-index/src/adjacency.rs`
- Modify: `crates/oxibrain-index/src/lib.rs`

**Interfaces:**
- Consumes: `index::vector::{TfIdfVector, cosine_sim}`, `core::retrieval::{Direction, PredicateFilter}`.
- Produces: `index::knn::KnnIndex`, `index::adjacency::{AdjacencyGraph, BfsSpec, BfsResult}`.

- [ ] **Step 1: Write knn.rs**

```rust
//! In-memory kNN index. Brute-force cosine similarity for M2 (deterministic).

use crate::vector::{TfIdfVector, cosine_sim};

pub struct KnnIndex {
    entries: Vec<(String, TfIdfVector)>,
}

impl KnnIndex {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn insert(&mut self, id: String, vector: TfIdfVector) {
        self.entries.push((id, vector));
    }

    /// Search for the k nearest neighbors. Returns (id, similarity) pairs
    /// sorted by similarity descending. Deterministic (brute-force, sorted
    /// by score desc then id asc).
    pub fn search(&self, query: &TfIdfVector, k: usize) -> Vec<(String, f64)> {
        let mut scored: Vec<(String, f64)> = self
            .entries
            .iter()
            .map(|(id, vec)| (id.clone(), cosine_sim(query, vec)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for KnnIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::TfIdfModel;

    #[test]
    fn search_returns_at_most_k() {
        let model = TfIdfModel::fit(&["alpha", "beta", "gamma"], 64);
        let mut index = KnnIndex::new();
        index.insert("d1".into(), model.transform("alpha"));
        index.insert("d2".into(), model.transform("beta"));
        index.insert("d3".into(), model.transform("gamma"));
        let results = index.search(&model.transform("alpha"), 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn nearest_neighbor_is_self() {
        let model = TfIdfModel::fit(&["hello world"], 64);
        let mut index = KnnIndex::new();
        index.insert("d1".into(), model.transform("hello world"));
        let results = index.search(&model.transform("hello world"), 1);
        assert_eq!(results[0].0, "d1");
    }
}
```

- [ ] **Step 2: Write adjacency.rs**

```rust
//! Adjacency graph view over statements (subject→object edges). Pure data
//! structure for BFS traversal.

use oxibrain_core::retrieval::{Direction, PredicateFilter};
use std::collections::{BTreeMap, BTreeSet};

pub struct AdjacencyGraph {
    nodes: BTreeSet<String>,
    /// outgoing: entity → [(neighbor, predicate, statement_id)]
    outgoing: BTreeMap<String, Vec<(String, String, String)>>,
    /// incoming: entity → [(neighbor, predicate, statement_id)]
    incoming: BTreeMap<String, Vec<(String, String, String)>>,
}

impl AdjacencyGraph {
    pub fn new() -> Self {
        Self {
            nodes: BTreeSet::new(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
        }
    }

    pub fn add_edge(&mut self, from: &str, to: &str, predicate: &str, stmt: &str) {
        self.nodes.insert(from.to_string());
        self.nodes.insert(to.to_string());
        self.outgoing
            .entry(from.to_string())
            .or_default()
            .push((to.to_string(), predicate.to_string(), stmt.to_string()));
        self.incoming
            .entry(to.to_string())
            .or_default()
            .push((from.to_string(), predicate.to_string(), stmt.to_string()));
    }

    pub fn neighbors_out(&self, entity: &str) -> &[(String, String, String)] {
        self.outgoing
            .get(entity)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn neighbors_in(&self, entity: &str) -> &[(String, String, String)] {
        self.incoming
            .get(entity)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// BFS traversal with bounds. Deterministic: processes nodes in sorted
    /// (BTreeSet) order at each depth level.
    pub fn bfs(&self, spec: &BfsSpec) -> BfsResult {
        let mut visited: BTreeMap<String, u8> = BTreeMap::new();
        let mut edges: Vec<(String, String, String, String, u8)> = Vec::new();
        let mut truncated = false;

        // Initialize queue with start nodes at depth 0.
        let mut frontier: BTreeSet<String> = spec.start.iter().cloned().collect();
        for s in &frontier {
            visited.insert(s.clone(), 0);
        }

        for depth in 1..=spec.max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next_frontier: BTreeSet<String> = BTreeSet::new();
            for entity in &frontier {
                if visited.len() as u32 >= spec.max_nodes {
                    truncated = true;
                    break;
                }
                let neighbors = match spec.direction {
                    Direction::Out => self.neighbors_out(entity),
                    Direction::In => self.neighbors_in(entity),
                    Direction::Both => {
                        // Merge out + in. We need owned data to filter.
                        let mut combined: Vec<&(String, String, String)> = Vec::new();
                        combined.extend(self.neighbors_out(entity).iter());
                        combined.extend(self.neighbors_in(entity).iter());
                        // Can't return a reference to combined, so handle inline.
                        for (neighbor, predicate, stmt) in &combined {
                            if !spec.predicate_filter.allows(predicate) {
                                continue;
                            }
                            edges.push((
                                entity.clone(),
                                neighbor.clone(),
                                predicate.clone(),
                                stmt.clone(),
                                depth,
                            ));
                            if !visited.contains_key(neighbor) {
                                if visited.len() as u32 >= spec.max_nodes {
                                    truncated = true;
                                    break;
                                }
                                visited.insert(neighbor.clone(), depth);
                                next_frontier.insert(neighbor.clone());
                            }
                        }
                        continue;
                    }
                };
                for (neighbor, predicate, stmt) in neighbors {
                    if !spec.predicate_filter.allows(predicate) {
                        continue;
                    }
                    edges.push((
                        entity.clone(),
                        neighbor.clone(),
                        predicate.clone(),
                        stmt.clone(),
                        depth,
                    ));
                    if !visited.contains_key(neighbor) {
                        if visited.len() as u32 >= spec.max_nodes {
                            truncated = true;
                            break;
                        }
                        visited.insert(neighbor.clone(), depth);
                        next_frontier.insert(neighbor.clone());
                    }
                }
                if truncated {
                    break;
                }
            }
            frontier = next_frontier;
            if truncated {
                break;
            }
        }

        BfsResult {
            nodes: visited,
            edges,
            truncated,
        }
    }
}

impl Default for AdjacencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BfsSpec {
    pub start: Vec<String>,
    pub max_depth: u8,
    pub max_nodes: u32,
    pub direction: Direction,
    pub predicate_filter: PredicateFilter,
}

pub struct BfsResult {
    pub nodes: BTreeMap<String, u8>,
    pub edges: Vec<(String, String, String, String, u8)>,
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_graph() -> AdjacencyGraph {
        let mut g = AdjacencyGraph::new();
        g.add_edge("a", "b", "knows", "s1");
        g.add_edge("b", "c", "knows", "s2");
        g.add_edge("c", "d", "knows", "s3");
        g
    }

    #[test]
    fn bfs_depth_2_reaches_c() {
        let g = chain_graph();
        let spec = BfsSpec {
            start: vec!["a".into()],
            max_depth: 2,
            max_nodes: 256,
            direction: Direction::Out,
            predicate_filter: PredicateFilter::AllowAll,
        };
        let result = g.bfs(&spec);
        assert!(result.nodes.contains_key("a"));
        assert!(result.nodes.contains_key("b"));
        assert!(result.nodes.contains_key("c"));
        assert!(!result.nodes.contains_key("d"));
        assert!(!result.truncated);
    }

    #[test]
    fn bfs_max_nodes_truncates() {
        let g = chain_graph();
        let spec = BfsSpec {
            start: vec!["a".into()],
            max_depth: 5,
            max_nodes: 2,
            direction: Direction::Out,
            predicate_filter: PredicateFilter::AllowAll,
        };
        let result = g.bfs(&spec);
        assert!(result.truncated);
        assert!(result.nodes.len() <= 3); // start + 1-2 more
    }

    #[test]
    fn predicate_filter_deny_blocks() {
        let g = chain_graph();
        let spec = BfsSpec {
            start: vec!["a".into()],
            max_depth: 3,
            max_nodes: 256,
            direction: Direction::Out,
            predicate_filter: PredicateFilter::Deny(vec!["knows".into()]),
        };
        let result = g.bfs(&spec);
        assert_eq!(result.nodes.len(), 1); // only the start node
    }
}
```

- [ ] **Step 3: Update index/lib.rs**

```rust
pub mod knn;
pub mod adjacency;

pub use knn::KnnIndex;
pub use adjacency::{AdjacencyGraph, BfsResult, BfsSpec};
```

- [ ] **Step 4: Verify**

```bash
cargo test -p oxibrain-index
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): kNN index + adjacency graph with bounded BFS"
```

---

## Task 4: Label propagation community clustering (index)

**Files:**
- Create: `crates/oxibrain-index/src/community.rs`
- Modify: `crates/oxibrain-index/src/lib.rs`

**Interfaces:**
- Consumes: `index::adjacency::AdjacencyGraph`.
- Produces: `index::community::{label_propagation, CommunityMap}`.

- [ ] **Step 1: Write community.rs**

```rust
//! Deterministic label-propagation community detection (DESIGN §9.4).
//! Tie-break: lowest label value wins. Fixed iteration cap guarantees termination.

use crate::adjacency::AdjacencyGraph;
use std::collections::BTreeMap;

/// entity_id → community label
#[derive(Debug, Clone)]
pub struct CommunityMap {
    pub labels: BTreeMap<String, u64>,
}

/// Run label propagation. Deterministic: entities processed in sorted order,
/// ties broken by lowest label, fixed iteration cap.
pub fn label_propagation(graph: &AdjacencyGraph, max_iterations: usize) -> CommunityMap {
    // Collect all node ids in sorted order.
    let nodes: Vec<String> = graph.all_nodes();

    // Initialize labels: each node gets a unique label = its ordinal position.
    let mut labels: BTreeMap<String, u64> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u64))
        .collect();

    for _ in 0..max_iterations {
        let mut changed = false;
        for node in &nodes {
            // Gather neighbor labels.
            let out = graph.neighbors_out(node);
            let in_ = graph.neighbors_in(node);
            let mut neighbor_labels: Vec<u64> = Vec::new();
            for (n, _, _) in out {
                if let Some(&l) = labels.get(n) {
                    neighbor_labels.push(l);
                }
            }
            for (n, _, _) in in_ {
                if let Some(&l) = labels.get(n) {
                    neighbor_labels.push(l);
                }
            }
            if neighbor_labels.is_empty() {
                continue;
            }
            // Find the most frequent label. Ties broken by lowest label value.
            neighbor_labels.sort_unstable();
            let new_label = most_frequent_with_lowest_tiebreak(&neighbor_labels);
            if labels.get(node) != Some(&new_label) {
                labels.insert(node.clone(), new_label);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    CommunityMap { labels }
}

/// Given a sorted slice of labels, return the most frequent one.
/// Ties broken by lowest value. Both guaranteed by the sorted input:
/// group by value, pick the group with the highest count; if counts tie,
/// the first group (lowest value) wins.
fn most_frequent_with_lowest_tiebreak(sorted: &[u64]) -> u64 {
    let mut best_label = sorted[0];
    let mut best_count = 1;
    let mut current_label = sorted[0];
    let mut current_count = 1;
    for &label in &sorted[1..] {
        if label == current_label {
            current_count += 1;
        } else {
            if current_count > best_count {
                best_count = current_count;
                best_label = current_label;
            }
            current_label = label;
            current_count = 1;
        }
    }
    if current_count > best_count {
        best_label = current_label;
    }
    best_label
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjacency::AdjacencyGraph;

    #[test]
    fn two_clusters_separate() {
        let mut g = AdjacencyGraph::new();
        // Cluster 1: a-b-c fully connected
        g.add_edge("a", "b", "knows", "s1");
        g.add_edge("b", "c", "knows", "s2");
        g.add_edge("a", "c", "knows", "s3");
        // Cluster 2: x-y-z fully connected
        g.add_edge("x", "y", "knows", "s4");
        g.add_edge("y", "z", "knows", "s5");
        g.add_edge("x", "z", "knows", "s6");
        let map = label_propagation(&g, 10);
        let label_a = map.labels["a"];
        let label_x = map.labels["x"];
        assert_ne!(label_a, label_x, "clusters should have different labels");
        assert_eq!(map.labels["a"], map.labels["b"]);
        assert_eq!(map.labels["b"], map.labels["c"]);
        assert_eq!(map.labels["x"], map.labels["y"]);
        assert_eq!(map.labels["y"], map.labels["z"]);
    }

    #[test]
    fn deterministic_across_runs() {
        let mut g = AdjacencyGraph::new();
        g.add_edge("a", "b", "knows", "s1");
        g.add_edge("b", "c", "knows", "s2");
        g.add_edge("c", "d", "knows", "s3");
        let m1 = label_propagation(&g, 10);
        let m2 = label_propagation(&g, 10);
        assert_eq!(m1.labels, m2.labels);
    }

    #[test]
    fn isolated_node_is_singleton() {
        let g = AdjacencyGraph::new();
        let map = label_propagation(&g, 10);
        assert!(map.labels.is_empty());
    }
}
```

Note: `AdjacencyGraph::all_nodes()` does not exist yet — add it to adjacency.rs.

- [ ] **Step 2: Add `all_nodes` to adjacency.rs**

Add this method to `AdjacencyGraph` in `crates/oxibrain-index/src/adjacency.rs`:

```rust
    pub fn all_nodes(&self) -> Vec<String> {
        self.nodes.iter().cloned().collect()
    }
```

- [ ] **Step 3: Update index/lib.rs**

```rust
pub mod community;
pub use community::{CommunityMap, label_propagation};
```

- [ ] **Step 4: Verify**

```bash
cargo test -p oxibrain-index
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): deterministic label-propagation community clustering"
```

---

## Task 5: v3 migration + index rebuild orchestration (store)

**Files:**
- Create: `crates/oxibrain-store/src/migrations/v3.sql`
- Modify: `crates/oxibrain-store/src/schema.rs`
- Modify: `crates/oxibrain-store/src/migration.rs`
- Create: `crates/oxibrain-store/src/index_ops.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`

**Interfaces:**
- Produces: `store::index_ops::{rebuild_indexes, render_statement,
  update_fts_for_episode}`.

- [ ] **Step 1: Write v3.sql**

`crates/oxibrain-store/src/migrations/v3.sql`:
```sql
-- FTS5 virtual table for lexical search over episode content + statement renderings.
CREATE VIRTUAL TABLE IF NOT EXISTS episodes_fts USING fts5(
    space_id UNINDEXED,
    target_kind,
    target_id,
    body,
    tokenize = 'porter unicode61'
);

-- TF-IDF vector storage (BLOB-serialized Vec<f32>).
CREATE TABLE IF NOT EXISTS tfidf_vectors (
    space_id    TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    vector      BLOB NOT NULL,
    PRIMARY KEY (space_id, target_kind, target_id)
);

-- Salience cache columns on entities.
ALTER TABLE entities ADD COLUMN salience REAL NOT NULL DEFAULT 1.0;
ALTER TABLE entities ADD COLUMN last_activity INTEGER;

-- Compaction columns on episodes.
ALTER TABLE episodes ADD COLUMN content_compacted BLOB;
ALTER TABLE episodes ADD COLUMN compacted_at INTEGER;
```

- [ ] **Step 2: Bump schema version**

Modify `crates/oxibrain-store/src/schema.rs`:
```rust
pub const LEDGER_SCHEMA_VERSION: i64 = 3;
```

- [ ] **Step 3: Add v3 migration step**

In `crates/oxibrain-store/src/migration.rs`, after the `current < 2` block and
before the final `user_version` read, add:

```rust
    if current < 3 {
        let sql = include_str!("migrations/v3.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 3i64)
            .map_err(sql_err)?;
    }
```

Also update the `newer_db_is_hard_error` test: change `expected: 2` to
`expected: 3`.

- [ ] **Step 4: Write index_ops.rs**

`crates/oxibrain-store/src/index_ops.rs`:
```rust
//! Index orchestration: FTS5 population, TF-IDF model build, vector persistence.
//! All operations are deterministic functions of the projection data.

use crate::sql_err;
use oxibrain_core::knowledge::{Object, Statement};
use oxibrain_core::object_repr;
use oxibrain_index::{TfIdfModel, TfIdfVector, tokenize};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Render a statement as a searchable string: "subject predicate object".
/// Uses entity surface names from entity_keys (canonical key).
pub fn render_statement(
    conn: &Connection,
    stmt: &Statement,
) -> Result<String, BrainError> {
    let subject_name = entity_surface(conn, &stmt.subject)?;
    let object_str = match &stmt.object {
        Object::Entity(eid) => entity_surface(conn, eid)?,
        Object::Literal(tv) => object_repr(&Object::Literal(tv.clone())),
    };
    Ok(format!("{subject_name} {} {object_str}", stmt.predicate))
}

fn entity_surface(conn: &Connection, entity_id: &str) -> Result<String, BrainError> {
    // Get the canonical key surface form, or fall back to the entity id.
    let row: Option<(Option<String>,)> = conn
        .query_row(
            "SELECT e.canonical_key
             FROM entities e
             WHERE e.id = ?1",
            params![entity_id],
            |r| Ok((r.get::<_, Option<String>>(0)?,)),
        )
        .map(Some)
        .map_err(sql_err)?;
    match row {
        Some((Some(key_id),)) => {
            let surface: Option<String> = conn
                .query_row(
                    "SELECT surface FROM entity_keys WHERE id = ?1",
                    params![key_id],
                    |r| r.get(0),
                )
                .map_err(sql_err)?;
            Ok(surface.unwrap_or_else(|| entity_id.to_string()))
        }
        _ => {
            // No canonical key — use the first surface from entity_keys.
            let surface: Option<String> = conn
                .query_row(
                    "SELECT surface FROM entity_keys WHERE entity_id = ?1 LIMIT 1",
                    params![entity_id],
                    |r| r.get(0),
                )
                .map_err(sql_err)?;
            Ok(surface.unwrap_or_else(|| entity_id.to_string()))
        }
    }
}

/// Drop and rebuild all FTS5 content for a space.
pub fn rebuild_fts(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute("DELETE FROM episodes_fts WHERE space_id = ?1", params![space])
        .map_err(sql_err)?;
    // Index episodes.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL",
        )
        .map_err(sql_err)?;
    let episodes: Vec<(String, String)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    for (id, content) in &episodes {
        conn.execute(
            "INSERT INTO episodes_fts (space_id, target_kind, target_id, body)
             VALUES (?1, 'episode', ?2, ?3)",
            params![space, id, content],
        )
        .map_err(sql_err)?;
    }
    // Index statement renderings.
    let statements = load_statements(conn, space)?;
    for stmt in &statements {
        let body = render_statement(conn, stmt)?;
        conn.execute(
            "INSERT INTO episodes_fts (space_id, target_kind, target_id, body)
             VALUES (?1, 'statement', ?2, ?3)",
            params![space, stmt.id, body],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Build TF-IDF model and persist vectors for all episodes + statements in a space.
pub fn rebuild_tfidf(conn: &Connection, space: &str, dim: usize) -> Result<(), BrainError> {
    // Collect all texts.
    let mut texts: Vec<String> = Vec::new();
    let mut targets: Vec<(&str, String)> = Vec::new(); // (kind, id)

    let mut stmt = conn
        .prepare("SELECT id, content FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL")
        .map_err(sql_err)?;
    let episodes: Vec<(String, String)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    for (id, content) in &episodes {
        texts.push(content.clone());
        targets.push(("episode", id.clone()));
    }

    let statements = load_statements(conn, space)?;
    for s in &statements {
        let body = render_statement(conn, s)?;
        texts.push(body);
        targets.push(("statement", s.id.clone()));
    }

    // Fit model.
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let model = TfIdfModel::fit(&text_refs, dim);

    // Persist vectors.
    conn.execute(
        "DELETE FROM tfidf_vectors WHERE space_id = ?1",
        params![space],
    )
    .map_err(sql_err)?;
    for ((kind, id), text) in targets.iter().zip(texts.iter()) {
        let vector = model.transform(text);
        conn.execute(
            "INSERT OR REPLACE INTO tfidf_vectors (space_id, target_kind, target_id, vector)
             VALUES (?1, ?2, ?3, ?4)",
            params![space, kind, id, vector.to_bytes()],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Rebuild salience cache (last_activity per entity).
pub fn rebuild_salience(conn: &Connection, space: &str) -> Result<(), BrainError> {
    conn.execute(
        "UPDATE entities SET last_activity = (
            SELECT MAX(a.recorded_at)
            FROM assertions a
            JOIN statements s ON a.statement_id = s.id
            WHERE s.subject_id = entities.id AND s.space_id = ?1
         ) WHERE entities.space_id = ?1",
        params![space],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Full index rebuild for a space: FTS + TF-IDF + salience.
pub fn rebuild_indexes(conn: &Connection, space: &str) -> Result<(), BrainError> {
    rebuild_fts(conn, space)?;
    rebuild_tfidf(conn, space, 1024)?;
    rebuild_salience(conn, space)?;
    Ok(())
}

/// Load all statements for a space (for rendering/indexing).
fn load_statements(conn: &Connection, space: &str) -> Result<Vec<Statement>, BrainError> {
    use oxibrain_core::knowledge::{Object, TypedValue};
    let mut stmt = conn
        .prepare(
            "SELECT id, subject_id, predicate, object_entity, object_literal
             FROM statements WHERE space_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            let object_entity: Option<String> = r.get(3)?;
            let object_literal: Option<String> = r.get(4)?;
            let object = match (object_entity, object_literal) {
                (Some(eid), None) => Object::Entity(eid),
                (None, Some(lit)) => Object::Literal(TypedValue::Text(lit)),
                _ => Object::Literal(TypedValue::Text(String::new())),
            };
            Ok(Statement {
                id: r.get(0)?,
                space: space.to_string(),
                subject: r.get(1)?,
                predicate: r.get(2)?,
                object,
            })
        })
        .map_err(sql_err)?;
    let mut statements = Vec::new();
    for row in rows {
        statements.push(row.map_err(sql_err)?);
    }
    Ok(statements)
}
```

- [ ] **Step 5: Register index_ops module**

In `crates/oxibrain-store/src/lib.rs`, add:
```rust
pub mod index_ops;
```

- [ ] **Step 6: Verify**

```bash
cargo test -p oxibrain-store
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(m2): v3 migration + FTS5/TF-IDF index rebuild orchestration"
```

---

## Task 6: Lexical + semantic search functions (store)

**Files:**
- Modify: `crates/oxibrain-store/src/query.rs`

**Interfaces:**
- Consumes: `store::index_ops`, `index::{KnnIndex, TfIdfModel, TfIdfVector}`.
- Produces: `query::{fts_search, semantic_search, load_knn_index, load_tfidf_model}`.

- [ ] **Step 1: Add search functions to query.rs**

Add to `crates/oxibrain-store/src/query.rs`:

```rust
use crate::index_ops;
use crate::sql_err;
use oxibrain_core::retrieval::{SearchHit, SearchTarget};
use oxibrain_index::{KnnIndex, TfIdfModel, TfIdfVector};

/// FTS5/BM25 lexical search. Returns hits sorted by BM25 score descending.
pub fn fts_search(
    conn: &Connection,
    space: &str,
    query_text: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, BrainError> {
    // Transform query into FTS5 syntax: space-separated terms (implicit AND).
    let fts_query: String = query_text
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT target_kind, target_id, rank
             FROM episodes_fts
             WHERE episodes_fts MATCH ?1 AND space_id = ?2
             ORDER BY rank
             LIMIT ?3",
        )
        .map_err(sql_err)?;
    let hits = stmt
        .query_map(params![&fts_query, space, limit as i64], |r| {
            let kind: String = r.get(0)?;
            let id: String = r.get(1)?;
            let rank: f64 = r.get(2)?;
            let target = match kind.as_str() {
                "episode" => SearchTarget::Episode { id },
                "statement" => SearchTarget::Statement { id },
                "entity" => SearchTarget::Entity { id },
                _ => SearchTarget::Episode { id },
            };
            // FTS5 rank: lower = better. Negate so higher = better.
            Ok(SearchHit {
                target,
                score: -rank,
                mode: oxibrain_core::retrieval::QueryMode::Lexical,
            })
        })
        .map_err(sql_err)?;
    let mut results = Vec::new();
    for hit in hits {
        results.push(hit.map_err(sql_err)?);
    }
    Ok(results)
}

/// Load the TF-IDF model for a space from the indexed texts.
pub fn load_tfidf_model(
    conn: &Connection,
    space: &str,
    dim: usize,
) -> Result<TfIdfModel, BrainError> {
    let mut stmt = conn
        .prepare("SELECT content FROM episodes WHERE space_id = ?1 AND redacted_at IS NULL")
        .map_err(sql_err)?;
    let texts: Vec<String> = stmt
        .query_map(params![space], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    Ok(TfIdfModel::fit(&text_refs, dim))
}

/// Load all persisted vectors into a KnnIndex.
pub fn load_knn_index(
    conn: &Connection,
    space: &str,
) -> Result<KnnIndex, BrainError> {
    let mut index = KnnIndex::new();
    let mut stmt = conn
        .prepare(
            "SELECT target_kind, target_id, vector FROM tfidf_vectors WHERE space_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            let kind: String = r.get(0)?;
            let id: String = r.get(1)?;
            let vector_blob: Vec<u8> = r.get(2)?;
            Ok((kind, id, vector_blob))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (kind, id, blob) = row.map_err(sql_err)?;
        let key = format!("{kind}:{id}");
        let vector = TfIdfVector::from_bytes(&blob);
        index.insert(key, vector);
    }
    Ok(index)
}

/// Semantic (TF-IDF kNN) search.
pub fn semantic_search(
    conn: &Connection,
    space: &str,
    query_text: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, BrainError> {
    let model = load_tfidf_model(conn, space, 1024)?;
    let query_vec = model.transform(query_text);
    let index = load_knn_index(conn, space)?;
    let results = index.search(&query_vec, limit);
    let hits = results
        .into_iter()
        .map(|(key, score)| {
            let (kind, id) = key.split_once(':').unwrap_or(("episode", &key));
            let target = match kind {
                "statement" => SearchTarget::Statement { id: id.to_string() },
                "entity" => SearchTarget::Entity { id: id.to_string() },
                _ => SearchTarget::Episode { id: id.to_string() },
            };
            SearchHit { target, score, mode: oxibrain_core::retrieval::QueryMode::Semantic }
        })
        .collect();
    Ok(hits)
}
```

- [ ] **Step 2: Write an integration test**

Create `crates/oxibrain-store/tests/search.rs`:
```rust
//! Integration test: FTS5 and semantic search over a hand-built graph.

use oxibrain_store::Store;
use tempfile::tempdir;

fn setup_store() -> (tempfile::TempDir, Store) {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    (dir, store)
}

// This test requires declaration plumbing (M1). It's a smoke test that the
// search functions compile and run against a migrated DB.
#[test]
fn fts_search_empty_space_returns_empty() {
    let (_dir, store) = setup_store();
    let conn = store.connection();
    let hits = oxibrain_store::query::fts_search(conn, "nonexistent", "test", 10)
        .expect("fts_search");
    assert!(hits.is_empty());
}
```

- [ ] **Step 3: Verify**

```bash
cargo test -p oxibrain-store --test search
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m2): FTS5 lexical + TF-IDF semantic search functions"
```

---

## Task 7: Hybrid query with RRF fusion + Brain::query

**Files:**
- Modify: `crates/oxibrain-store/src/query.rs` (add `hybrid_query`)
- Modify: `crates/oxibrain/src/lib.rs` (add `Brain::query`)

**Interfaces:**
- Consumes: `query::{fts_search, semantic_search}`, `index::rrf::fuse`.
- Produces: `query::hybrid_query`, `Brain::query`.

- [ ] **Step 1: Add hybrid_query to store/query.rs**

```rust
use oxibrain_core::retrieval::{
    DroppedItem, DropReason, Query, QueryMode, RankedItem, RankingResult,
};
use oxibrain_index::rrf;

/// Convert a SearchHit to an RRF composite key.
fn hit_key(hit: &SearchHit) -> String {
    match &hit.target {
        SearchTarget::Episode { id } => format!("episode:{id}"),
        SearchTarget::Statement { id } => format!("statement:{id}"),
        SearchTarget::Entity { id } => format!("entity:{id}"),
    }
}

/// Parse an RRF composite key back into a SearchTarget.
fn parse_key(key: &str) -> SearchTarget {
    match key.split_once(':') {
        Some(("statement", id)) => SearchTarget::Statement { id: id.to_string() },
        Some(("entity", id)) => SearchTarget::Entity { id: id.to_string() },
        _ => SearchTarget::Episode { id: key.to_string() },
    }
}

/// Hybrid query: run all enabled modes, fuse with RRF, enrich with salience.
pub fn hybrid_query(
    conn: &Connection,
    q: &Query,
) -> Result<RankingResult, BrainError> {
    let limit = q.limit;
    let mut mode_lists: Vec<Vec<SearchHit>> = Vec::new();

    let run_lexical = matches!(q.mode, QueryMode::Hybrid | QueryMode::Lexical);
    let run_semantic = matches!(q.mode, QueryMode::Hybrid | QueryMode::Semantic);
    let run_graph = matches!(q.mode, QueryMode::Hybrid | QueryMode::Graph);

    let mut dropped: Vec<DroppedItem> = Vec::new();

    if run_lexical {
        let hits = fts_search(conn, &q.space, &q.text, limit)?;
        mode_lists.push(hits);
    }
    if run_semantic {
        let hits = semantic_search(conn, &q.space, &q.text, limit)?;
        mode_lists.push(hits);
    }
    if run_graph {
        // Graph mode: find entities mentioned in the query text via lexical,
        // then expand their neighbors. For M2 simplicity, this reuses lexical
        // entity hits and returns them as graph hits.
        let hits = fts_search(conn, &q.space, &q.text, limit)?
            .into_iter()
            .filter(|h| matches!(h.target, SearchTarget::Entity { .. }))
            .map(|mut h| {
                h.mode = QueryMode::Graph;
                h
            })
            .collect::<Vec<_>>();
        if !hits.is_empty() {
            mode_lists.push(hits);
        }
    }

    // Convert to RRF input format.
    let rrf_lists: Vec<Vec<(String, f64)>> = mode_lists
        .iter()
        .map(|hits| hits.iter().map(|h| (hit_key(h), h.score)).collect())
        .collect();

    let fused = rrf::fuse(&rrf_lists, 60);

    let total_found = fused.len();
    let items: Vec<RankedItem> = fused
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(rank, item)| {
            // Compute mode ranks for this item.
            let mode_ranks: Vec<(QueryMode, usize)> = mode_lists
                .iter()
                .filter_map(|hits| {
                    hits.iter().position(|h| hit_key(h) == item.key).map(|pos| {
                        (hits.first().map(|h| h.mode).unwrap_or(QueryMode::Lexical), pos)
                    })
                })
                .collect();
            RankedItem {
                target: parse_key(&item.key),
                fused_score: item.score,
                rank,
                mode_ranks,
                salience: 1.0, // M2: salience default; decay recalculates
            }
        })
        .collect();

    // Record truncated items as dropped.
    if total_found > limit {
        // Items beyond the limit were truncated.
        // (Not individually tracked for M2 simplicity — the count is enough.)
    }

    Ok(RankingResult {
        items,
        dropped,
        total_found,
        query: q.clone(),
    })
}
```

- [ ] **Step 2: Add Brain::query to facade**

Add to `crates/oxibrain/src/lib.rs` `impl Brain`:

```rust
    /// Hybrid (or mode-specific) query. Returns ranked results with provenance.
    pub async fn query(
        &self,
        q: oxibrain_core::retrieval::Query,
    ) -> Result<oxibrain_core::retrieval::RankingResult, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| query::hybrid_query(conn, &q))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

- [ ] **Step 3: Write an integration test**

Create `crates/oxibrain/tests/m2_query.rs`:
```rust
//! M2 integration test: hybrid query over a hand-built graph.

use oxibrain::Brain;
use oxibrain_core::retrieval::{Query, QueryMode};
use oxibrain_ports::{ClockPort, FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use tempfile::tempdir;

fn decl_add(subj: &str, subj_ty: &str, pred: &str, obj: &str, obj_ty: &str) -> Declaration {
    Declaration::AddStatement {
        subject: EntityRef { surface: subj.into(), ty: subj_ty.into() },
        predicate: pred.into(),
        object: DeclObject::Entity { surface: obj.into(), ty: obj_ty.into() },
        polarity: "affirm".into(),
        valid_from: oxibrain_ports::TIME_MIN.millis(),
        valid_to: oxibrain_ports::TIME_MAX.millis(),
    }
}

#[tokio::test]
async fn hybrid_query_finds_declared_knowledge() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock.clone(),
    )
    .await
    .expect("open");

    let space = brain.ensure_space("test").await.expect("space");

    // Declare: Alice works_on ProjectX.
    brain
        .declare(&space, decl_add("Alice", "Person", "works_on", "ProjectX", "Project"))
        .await
        .expect("declare");

    // Rebuild indexes.
    {
        let h = brain.handle().clone();
        tokio::task::spawn_blocking(move || {
            h.writer.submit(Box::new(|conn| {
                oxibrain_store::index_ops::rebuild_indexes(conn, &space.clone())
            })).expect("submit");
            h.writer.flush().expect("flush");
        })
        .await
        .expect("join");
    }

    let q = Query {
        text: "Alice ProjectX".into(),
        mode: QueryMode::Hybrid,
        space: space.clone(),
        as_of: None,
        limit: 10,
        min_confidence: 0.0,
    };
    let result = brain.query(q).await.expect("query");
    assert!(!result.items.is_empty(), "should find results for declared knowledge");
}
```

Note: `brain.handle()` is not public. Use a store method or expose the handle.
**Fix:** Add `pub fn handle(&self) -> &Arc<StoreHandle>` to `Brain` (or make the
test rebuild indexes via a `Brain::rebuild_indexes` method). Add to facade:

```rust
    /// Rebuild all indexes for a space (FTS5, TF-IDF, salience).
    pub async fn rebuild_indexes(&self, space: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                oxibrain_store::index_ops::rebuild_indexes(conn, &space)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("rebuild_indexes channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

Use `brain.rebuild_indexes(&space).await` in the test instead of direct handle access.

- [ ] **Step 4: Verify**

```bash
cargo test -p oxibrain --test m2_query
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): hybrid query with RRF fusion + Brain::query"
```

---

## Task 8: Bounded traversal + Brain::traverse

**Files:**
- Modify: `crates/oxibrain-store/src/query.rs` (add `traverse`, `load_adjacency`)
- Modify: `crates/oxibrain/src/lib.rs` (add `Brain::traverse`)

**Interfaces:**
- Consumes: `index::adjacency::{AdjacencyGraph, BfsSpec}`, core `TraversalSpec`.
- Produces: `query::traverse`, `Brain::traverse`.

- [ ] **Step 1: Add traverse to store/query.rs**

```rust
use oxibrain_core::retrieval::{
    Direction, PredicateFilter, TraversalEdge, TraversalNode, TraversalResult,
    TraversalSpec,
};
use oxibrain_index::adjacency::{AdjacencyGraph, BfsSpec};

/// Load the adjacency graph from the statements table (entity→entity edges only).
pub fn load_adjacency(conn: &Connection, space: &str) -> Result<AdjacencyGraph, BrainError> {
    let mut graph = AdjacencyGraph::new();
    let mut stmt = conn
        .prepare(
            "SELECT subject_id, object_entity, predicate, id
             FROM statements
             WHERE space_id = ?1 AND object_entity IS NOT NULL",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        let (subj, obj, pred, id) = row.map_err(sql_err)?;
        graph.add_edge(&subj, &obj, &pred, &id);
    }
    Ok(graph)
}
```

```rust
/// Bounded BFS traversal over the adjacency graph.
pub fn traverse(
    conn: &Connection,
    space: &str,
    spec: &TraversalSpec,
) -> Result<TraversalResult, BrainError> {
    let graph = load_adjacency(conn, space)?;
    let bfs_spec = BfsSpec {
        start: spec.start.clone(),
        max_depth: spec.max_depth,
        max_nodes: spec.max_nodes,
        direction: spec.direction,
        predicate_filter: spec.predicates.clone(),
    };
    let bfs_result = graph.bfs(&bfs_spec);

    let nodes: Vec<TraversalNode> = bfs_result
        .nodes
        .iter()
        .map(|(entity, &depth)| TraversalNode {
            entity: entity.clone(),
            depth,
            salience: 1.0, // M2: default salience
        })
        .collect();
    let edges: Vec<TraversalEdge> = bfs_result
        .edges
        .into_iter()
        .map(|(from, to, predicate, statement_id, depth)| TraversalEdge {
            from,
            to,
            predicate,
            statement_id,
            depth,
        })
        .collect();

    Ok(TraversalResult {
        nodes,
        edges,
        truncated: bfs_result.truncated,
    })
}
```

- [ ] **Step 2: Add Brain::traverse to facade**

```rust
    /// Bounded subgraph traversal.
    pub async fn traverse(
        &self,
        space: &str,
        spec: oxibrain_core::retrieval::TraversalSpec,
    ) -> Result<oxibrain_core::retrieval::TraversalResult, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| query::traverse(conn, &space, &spec))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

- [ ] **Step 3: Write integration test**

Add to `crates/oxibrain/tests/m2_query.rs`:
```rust
#[tokio::test]
async fn traversal_finds_multihop() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");
    let space = brain.ensure_space("test").await.expect("space");

    // Declare a chain: A → B → C → D
    for (s, o) in [("A", "B"), ("B", "C"), ("C", "D")] {
        brain
            .declare(&space, decl_add(s, "Concept", "knows", o, "Concept"))
            .await
            .expect("declare");
    }

    // Need entity IDs. Compute from id derivation (same as projection pipeline).
    let entity_a = entity_id_for(&space, "Concept", "A");

    let spec = oxibrain_core::retrieval::TraversalSpec {
        start: vec![entity_a],
        max_depth: 3,
        max_nodes: 256,
        predicates: oxibrain_core::retrieval::PredicateFilter::AllowAll,
        direction: oxibrain_core::retrieval::Direction::Out,
        valid_at: None,
        min_confidence: 0.0,
        strategy: oxibrain_core::retrieval::Strategy::Bfs,
    };
    let result = brain.traverse(&space, spec).await.expect("traverse");
    assert!(result.nodes.len() >= 4, "should reach all 4 nodes: {:?}", result.nodes);
}

/// Compute the entity ID for a surface form using the same content-derived
/// id derivation as the projection pipeline (no DB access needed).
fn entity_id_for(space: &str, ty: &str, surface: &str) -> String {
    let normalized = oxibrain_core::resolution::normalize(surface);
    oxibrain_core::id::entity_id(space, ty, &normalized)
}
```

Then in the traversal test, replace `resolve_entity_id(&brain, &space, "A").await`
with `entity_id_for(&space, "Concept", "A")`.

- [ ] **Step 4: Verify**

```bash
cargo test -p oxibrain --test m2_query
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): bounded BFS traversal + Brain::traverse"
```

---

## Task 9: timeline + diff + why + why_dropped

**Files:**
- Create: `crates/oxibrain-store/src/timeline.rs`
- Create: `crates/oxibrain-store/src/explain.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`
- Modify: `crates/oxibrain/src/lib.rs`

**Interfaces:**
- Produces: `timeline::{timeline, diff}`, `explain::{why, explain_statement}`.

- [ ] **Step 1: Write timeline.rs**

`crates/oxibrain-store/src/timeline.rs`:
```rust
//! Timeline and diff queries (DESIGN §12.2, §9.6).

use crate::sql_err;
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub statement_id: String,
    pub predicate: String,
    pub object_repr: String,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub status: String,
    pub recorded_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResult {
    pub added: Vec<TimelineEntry>,
    pub removed: Vec<TimelineEntry>,
    pub changed: Vec<TimelineEntry>,
}

/// Belief intervals for an entity over [from, to].
pub fn timeline(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    from: Option<Timestamp>,
    to: Option<Timestamp>,
) -> Result<Vec<TimelineEntry>, BrainError> {
    let from_millis = from.map(|t| t.millis()).unwrap_or(i64::MIN);
    let to_millis = to.map(|t| t.millis()).unwrap_or(i64::MAX);
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.predicate, s.object_entity, s.object_literal,
                    b.valid_from, b.valid_to, b.status, b.confidence,
                    (SELECT MAX(a.recorded_at) FROM assertions a WHERE a.statement_id = s.id) AS recorded_at
             FROM beliefs b
             JOIN statements s ON b.statement_id = s.id
             WHERE s.space_id = ?1 AND s.subject_id = ?2
               AND b.valid_from <= ?3 AND b.valid_to >= ?4
             ORDER BY b.valid_from",
        )
        .map_err(sql_err)?;
    let entries = stmt
        .query_map(params![space, entity_id, to_millis, from_millis], |r| {
            let object_entity: Option<String> = r.get(2)?;
            let object_literal: Option<String> = r.get(3)?;
            let object_repr = object_entity.or(object_literal).unwrap_or_default();
            Ok(TimelineEntry {
                statement_id: r.get(0)?,
                predicate: r.get(1)?,
                object_repr,
                valid_from: Timestamp(r.get(4)?),
                valid_to: Timestamp(r.get(5)?),
                status: r.get(6)?,
                recorded_at: Timestamp(r.get(8)?),
            })
        })
        .map_err(sql_err)?;
    let mut results = Vec::new();
    for entry in entries {
        results.push(entry.map_err(sql_err)?);
    }
    Ok(results)
}

/// What changed for an entity between two time points.
pub fn diff(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    at_a: Timestamp,
    at_b: Timestamp,
) -> Result<DiffResult, BrainError> {
    let beliefs_a = beliefs_at(conn, space, entity_id, at_a)?;
    let beliefs_b = beliefs_at(conn, space, entity_id, at_b)?;
    let map_a: std::collections::HashMap<String, &TimelineEntry> =
        beliefs_a.iter().map(|e| (e.statement_id.clone(), e)).collect();
    let map_b: std::collections::HashMap<String, &TimelineEntry> =
        beliefs_b.iter().map(|e| (e.statement_id.clone(), e)).collect();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (id, entry_b) in &map_b {
        match map_a.get(id) {
            None => added.push((*entry_b).clone()),
            Some(entry_a) if entry_a.status != entry_b.status
                || entry_a.valid_from != entry_b.valid_from =>
            {
                changed.push((*entry_b).clone());
            }
            _ => {}
        }
    }
    for (id, entry_a) in &map_a {
        if !map_b.contains_key(id) {
            removed.push((*entry_a).clone());
        }
    }
    added.sort_by(|a, b| a.statement_id.cmp(&b.statement_id));
    removed.sort_by(|a, b| a.statement_id.cmp(&b.statement_id));
    changed.sort_by(|a, b| a.statement_id.cmp(&b.statement_id));
    Ok(DiffResult { added, removed, changed })
}

fn beliefs_at(
    conn: &Connection,
    space: &str,
    entity_id: &str,
    at: Timestamp,
) -> Result<Vec<TimelineEntry>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.predicate, s.object_entity, s.object_literal,
                    b.valid_from, b.valid_to, b.status,
                    (SELECT MAX(a.recorded_at) FROM assertions a WHERE a.statement_id = s.id)
             FROM beliefs b
             JOIN statements s ON b.statement_id = s.id
             WHERE s.space_id = ?1 AND s.subject_id = ?2
               AND b.valid_from <= ?3 AND b.valid_to >= ?3
             ORDER BY s.id",
        )
        .map_err(sql_err)?;
    let entries = stmt
        .query_map(params![space, entity_id, at.millis()], |r| {
            let object_entity: Option<String> = r.get(2)?;
            let object_literal: Option<String> = r.get(3)?;
            Ok(TimelineEntry {
                statement_id: r.get(0)?,
                predicate: r.get(1)?,
                object_repr: object_entity.or(object_literal).unwrap_or_default(),
                valid_from: Timestamp(r.get(4)?),
                valid_to: Timestamp(r.get(5)?),
                status: r.get(6)?,
                recorded_at: Timestamp(r.get(7)?),
            })
        })
        .map_err(sql_err)?;
    let mut results = Vec::new();
    for entry in entries {
        results.push(entry.map_err(sql_err)?);
    }
    Ok(results)
}
```

- [ ] **Step 2: Write explain.rs**

`crates/oxibrain-store/src/explain.rs`:
```rust
//! Explainability queries: why (provenance + confidence breakdown).

use crate::sql_err;
use oxibrain_core::knowledge::{Object, Statement};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainBlock {
    pub statement: Statement,
    pub status: String,
    pub assertions: Vec<AssertionDetail>,
    pub confidence_breakdown: ConfidenceBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionDetail {
    pub assertion_id: String,
    pub episode_id: String,
    pub extractor: Option<String>,
    pub polarity: String,
    pub confidence: f32,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceBreakdown {
    pub raw_confidence: f32,
    pub support_count: usize,
    pub contradiction_count: usize,
}

/// Provenance and confidence breakdown for a statement.
pub fn why(conn: &Connection, space: &str, statement_id: &str) -> Result<ExplainBlock, BrainError> {
    let stmt_row = conn
        .query_row(
            "SELECT id, subject_id, predicate, object_entity, object_literal
             FROM statements WHERE space_id = ?1 AND id = ?2",
            params![space, statement_id],
            |r| {
                let object_entity: Option<String> = r.get(3)?;
                let object_literal: Option<String> = r.get(4)?;
                let object = match (object_entity, object_literal) {
                    (Some(eid), None) => Object::Entity(eid),
                    (None, Some(lit)) => Object::Literal(oxibrain_core::TypedValue::Text(lit)),
                    _ => Object::Literal(oxibrain_core::TypedValue::Text(String::new())),
                };
                Ok(Statement {
                    id: r.get(0)?,
                    space: space.to_string(),
                    subject: r.get(1)?,
                    predicate: r.get(2)?,
                    object,
                })
            },
        )
        .map_err(sql_err)?;

    let status: String = conn
        .query_row(
            "SELECT status FROM beliefs WHERE statement_id = ?1 ORDER BY valid_from DESC LIMIT 1",
            params![statement_id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "unknown".to_string());

    let mut assert_stmt = conn
        .prepare(
            "SELECT id, episode_id, extractor_id, polarity, confidence, recorded_at
             FROM assertions WHERE statement_id = ?1 ORDER BY recorded_at",
        )
        .map_err(sql_err)?;
    let details = assert_stmt
        .query_map(params![statement_id], |r| {
            let polarity_int: i64 = r.get(3)?;
            let polarity = if polarity_int == 0 { "affirm" } else { "deny" };
            Ok(AssertionDetail {
                assertion_id: r.get(0)?,
                episode_id: r.get(1)?,
                extractor: r.get(2)?,
                polarity: polarity.to_string(),
                confidence: r.get(4)?,
                recorded_at: r.get(5)?,
            })
        })
        .map_err(sql_err)?;
    let mut assertions = Vec::new();
    for d in details {
        assertions.push(d.map_err(sql_err)?);
    }

    let support_count = assertions.iter().filter(|a| a.polarity == "affirm").count();
    let contradiction_count = assertions.iter().filter(|a| a.polarity == "deny").count();
    let raw_confidence = assertions.first().map(|a| a.confidence).unwrap_or(0.0);

    Ok(ExplainBlock {
        statement: stmt_row,
        status,
        assertions,
        confidence_breakdown: ConfidenceBreakdown {
            raw_confidence,
            support_count,
            contradiction_count,
        },
    })
}
```

- [ ] **Step 3: Register modules + add facade methods**

In `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod timeline;
pub mod explain;
```

In `crates/oxibrain/src/lib.rs`, add:
```rust
    pub async fn timeline(
        &self,
        space: &str,
        entity_id: &str,
        from: Option<oxibrain_ports::Timestamp>,
        to: Option<oxibrain_ports::Timestamp>,
    ) -> Result<Vec<oxibrain_store::timeline::TimelineEntry>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| oxibrain_store::timeline::timeline(conn, &space, &entity_id, from, to))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn diff(
        &self,
        space: &str,
        entity_id: &str,
        at_a: oxibrain_ports::Timestamp,
        at_b: oxibrain_ports::Timestamp,
    ) -> Result<oxibrain_store::timeline::DiffResult, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| oxibrain_store::timeline::diff(conn, &space, &entity_id, at_a, at_b))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn why(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<oxibrain_store::explain::ExplainBlock, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| oxibrain_store::explain::why(conn, &space, &statement_id))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

- [ ] **Step 4: Write integration test**

Add to `crates/oxibrain/tests/m2_query.rs` a test that declares a supersession
scenario and checks timeline and why.

- [ ] **Step 5: Verify**

```bash
cargo test -p oxibrain --test m2_query
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(m2): timeline, diff, why explainability queries"
```

---

## Task 10: Community rebuild + community query

**Files:**
- Create: `crates/oxibrain-store/src/communities.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`
- Modify: `crates/oxibrain-store/src/query.rs` (add `community_query`)
- Modify: `crates/oxibrain/src/lib.rs` (add `Brain::rebuild_communities`)

**Interfaces:**
- Consumes: `index::community::label_propagation`, `query::load_adjacency`.
- Produces: `communities::rebuild_communities`, `query::community_query`.

- [ ] **Step 1: Write communities.rs**

```rust
//! Community clustering via label propagation (DESIGN §9.4).

use crate::query::load_adjacency;
use crate::sql_err;
use oxibrain_index::community::label_propagation;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Rebuild community assignments for a space. Deterministic.
pub fn rebuild_communities(conn: &Connection, space: &str) -> Result<(), BrainError> {
    let graph = load_adjacency(conn, space)?;
    let map = label_propagation(&graph, 10);
    // Clear and repopulate the communities table.
    conn.execute("DELETE FROM communities WHERE space_id = ?1", params![space])
        .map_err(sql_err)?;
    for (entity_id, label) in &map.labels {
        conn.execute(
            "INSERT OR REPLACE INTO communities (id, space_id, label) VALUES (?1, ?2, ?3)",
            params![entity_id, space, label],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Get all entities in the same community as `entity_id`.
pub fn community_members(
    conn: &Connection,
    space: &str,
    entity_id: &str,
) -> Result<Vec<String>, BrainError> {
    let label: Option<i64> = conn
        .query_row(
            "SELECT label FROM communities WHERE id = ?1 AND space_id = ?2",
            params![entity_id, space],
            |r| r.get(0),
        )
        .ok();
    let Some(label) = label else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare("SELECT id FROM communities WHERE space_id = ?1 AND label = ?2 ORDER BY id")
        .map_err(sql_err)?;
    let members = stmt
        .query_map(params![space, label], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    Ok(members)
}
```

- [ ] **Step 2: Register module + facade**

In `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod communities;
```

In facade, add:
```rust
    pub async fn rebuild_communities(&self, space: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                oxibrain_store::communities::rebuild_communities(conn, &space)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("rebuild_communities channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

- [ ] **Step 3: Write integration test**

Test: declare two separate clusters (A-B-C, X-Y-Z), rebuild communities, assert
members of A's community = {A,B,C} and X's community = {X,Y,Z}.

- [ ] **Step 4: Verify**

```bash
cargo test -p oxibrain --test m2_query
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): label-propagation community clustering + rebuild"
```

---

## Task 11: Salience decay + compaction

**Files:**
- Create: `crates/oxibrain-store/src/lifecycle.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`
- Modify: `crates/oxibrain/src/lib.rs`

**Interfaces:**
- Consumes: `core::lifecycle::{salience, DecayConfig, CompactionConfig}`,
  `store::index_ops::rebuild_salience`.
- Produces: `lifecycle::{apply_decay, compact_episodes}`.

- [ ] **Step 1: Write lifecycle.rs**

```rust
//! Lifecycle WriteOps: salience decay + compaction (DESIGN §10).

use crate::sql_err;
use oxibrain_core::lifecycle::{DecayConfig, salience};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, params};

/// Recalculate salience for all entities in a space using time-decay formula.
/// Returns the number of entities updated.
pub fn apply_decay(
    conn: &Connection,
    space: &str,
    now: Timestamp,
    config: &DecayConfig,
) -> Result<usize, BrainError> {
    // Read all entities with their last_activity.
    let mut stmt = conn
        .prepare(
            "SELECT id, last_activity FROM entities WHERE space_id = ?1",
        )
        .map_err(sql_err)?;
    let entities: Vec<(String, Option<i64>)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);

    let mut count = 0;
    for (entity_id, last_activity) in &entities {
        let last = last_activity
            .map(Timestamp)
            .unwrap_or(now);
        let salience_val = salience(last, now, config);
        conn.execute(
            "UPDATE entities SET salience = ?1 WHERE id = ?2",
            params![salience_val, entity_id],
        )
        .map_err(sql_err)?;
        count += 1;
    }
    Ok(count)
}

/// Compact cold episodes: compress content into content_compacted BLOB.
/// Returns the number of episodes compacted.
pub fn compact_episodes(
    conn: &Connection,
    space: &str,
    now: Timestamp,
    min_age_days: u32,
) -> Result<usize, BrainError> {
    let min_age_millis = (min_age_days as i64) * 86_400_000;
    let cutoff = now.millis() - min_age_millis;
    // Find episodes older than cutoff that haven't been compacted.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
             WHERE space_id = ?1
               AND compacted_at IS NULL
               AND ingested_at < ?2
               AND content != ''",
        )
        .map_err(sql_err)?;
    let candidates: Vec<(String, String)> = stmt
        .query_map(params![space, cutoff], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);

    let mut count = 0;
    for (id, content) in &candidates {
        // Compress with flate2 (zlib). For M2 simplicity, use std::io with
        // flate2 crate. If flate2 is not available, store raw as fallback.
        // For now, store the content as-is in the BLOB (no compression —
        // flate2 integration is a runtime detail; the interface is what matters).
        let compressed = content.as_bytes().to_vec();
        conn.execute(
            "UPDATE episodes SET content_compacted = ?1, compacted_at = ?2, content = ''
             WHERE id = ?3",
            params![compressed, now.millis(), id],
        )
        .map_err(sql_err)?;
        count += 1;
    }
    Ok(count)
}
```

Note: real compression needs `flate2` dependency. For M2, store uncompressed in
the BLOB; add `flate2` + compress when budget measurements show it's needed.
The interface (`content_compacted` BLOB, transparent decompression) is stable.

- [ ] **Step 2: Update ledger::get_episode for transparent decompression**

In `crates/oxibrain-store/src/ledger.rs`, update `get_episode` to check
`content_compacted`:

After loading the episode, if `content` is empty and `content_compacted` is
non-null, decompress (or copy) from the BLOB into `content`.

- [ ] **Step 3: Register module + facade methods**

In `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod lifecycle;
```

In facade:
```rust
    pub async fn apply_decay(&self, space: &str) -> Result<usize, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let now = self.clock.now();
        let config = oxibrain_core::lifecycle::DecayConfig::default();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let count = oxibrain_store::lifecycle::apply_decay(conn, &space, now, &config)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("apply_decay channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn compact(&self, space: &str) -> Result<usize, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let count = oxibrain_store::lifecycle::compact_episodes(conn, &space, now, 90)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("compact channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

- [ ] **Step 4: Write integration test**

Test: declare entities, apply_decay, assert salience decreased and respects floor.
Test: ingest an old episode, compact, assert content is retrievable.

- [ ] **Step 5: Verify**

```bash
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(m2): salience decay + episode compaction lifecycle ops"
```

---

## Task 12: assemble_context

**Files:**
- Create: `crates/oxibrain-store/src/context.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`
- Modify: `crates/oxibrain/src/lib.rs`

**Interfaces:**
- Consumes: `core::context::{ContextBudget, ContextResult, ContextLayer,
  LayerKind, estimate_tokens}`, `query::hybrid_query`.
- Produces: `context::assemble_context`.

- [ ] **Step 1: Write context.rs**

```rust
//! Context assembly: packing policy for agent memory (DESIGN §9.5).

use crate::query;
use crate::sql_err;
use oxibrain_core::context::{
    ContextBudget, ContextLayer, ContextResult, LayerKind, estimate_tokens,
};
use oxibrain_core::retrieval::{Query, QueryMode};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

/// Pack context for a query to a token budget.
pub fn assemble_context(
    conn: &Connection,
    space: &str,
    query_text: &str,
    budget: usize,
) -> Result<ContextResult, BrainError> {
    let mut layers: Vec<ContextLayer> = Vec::new();
    let mut total_tokens = 0;
    let mut truncated = false;

    // Layer 1: Pinned facts (M2: empty — pin API is M4).

    // Layer 2: High-salience beliefs for query-relevant entities.
    let q = Query {
        text: query_text.to_string(),
        mode: QueryMode::Lexical,
        space: space.to_string(),
        as_of: None,
        limit: 10,
        min_confidence: 0.0,
    };
    let ranking = query::hybrid_query(conn, &q)?;
    let mut beliefs_text = String::new();
    let mut beliefs_provenance: Vec<String> = Vec::new();
    for item in &ranking.items {
        if let oxibrain_core::retrieval::SearchTarget::Statement { id } = &item.target {
            let entry = render_belief(conn, space, id)?;
            beliefs_text.push_str(&entry.text);
            beliefs_text.push('\n');
            beliefs_provenance.push(id.clone());
        }
    }
    if !beliefs_text.is_empty() {
        let tokens = estimate_tokens(&beliefs_text);
        total_tokens += tokens;
        layers.push(ContextLayer {
            kind: LayerKind::HighSalienceBeliefs,
            text: beliefs_text,
            estimated_tokens: tokens,
            provenance: beliefs_provenance,
        });
    }

    // Layer 3: Query neighborhood (1-hop adjacency of top entities).
    // For M2, skip if no entities found.
    // (Would call query::load_adjacency + neighbors, but keeping M2 simple.)

    // Layer 4: Recent episodes.
    let mut stmt = conn
        .prepare(
            "SELECT id, content FROM episodes
             WHERE space_id = ?1 AND redacted_at IS NULL AND content != ''
             ORDER BY ingested_at DESC LIMIT 5",
        )
        .map_err(sql_err)?;
    let recent: Vec<(String, String)> = stmt
        .query_map(params![space], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(stmt);
    if !recent.is_empty() {
        let mut episode_text = String::new();
        let mut episode_prov: Vec<String> = Vec::new();
        for (id, content) in &recent {
            let remaining = budget.saturating_sub(total_tokens);
            if estimate_tokens(content) > remaining {
                truncated = true;
                break;
            }
            episode_text.push_str(content);
            episode_text.push('\n');
            episode_prov.push(id.clone());
            total_tokens += estimate_tokens(content);
        }
        if !episode_text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::RecentEpisodes,
                text: episode_text,
                estimated_tokens: estimate_tokens(&episode_text),
                provenance: episode_prov,
            });
        }
    }

    Ok(ContextResult {
        layers,
        total_tokens,
        budget: ContextBudget { max_tokens: budget },
        truncated,
    })
}

struct RenderedBelief { text: String }

fn render_belief(
    conn: &Connection,
    space: &str,
    statement_id: &str,
) -> Result<RenderedBelief, BrainError> {
    let row: (String, Option<String>, Option<String>, String, String, f64) = conn
        .query_row(
            "SELECT s.predicate, s.object_entity, s.object_literal,
                    b.status, b.valid_from, b.confidence
             FROM statements s
             LEFT JOIN beliefs b ON b.statement_id = s.id
             WHERE s.id = ?1 AND s.space_id = ?2
             ORDER BY b.valid_from DESC LIMIT 1",
            params![statement_id, space],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, f64>(5)?,
                ))
            },
        )
        .map_err(sql_err)?;
    let (predicate, obj_entity, obj_literal, status, _valid_from, confidence) = row;
    let object_repr = obj_entity.or(obj_literal).unwrap_or_default();
    let text = format!("... {predicate} {object_repr} (status={status}, confidence={confidence:.2})");
    Ok(RenderedBelief { text })
}
```

- [ ] **Step 2: Register module + facade**

In `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod context;
```

```rust
    pub async fn assemble_context(
        &self,
        space: &str,
        query: &str,
        token_budget: usize,
    ) -> Result<oxibrain_core::context::ContextResult, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::context::assemble_context(conn, &space, &query, token_budget)
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

- [ ] **Step 3: Write integration test**

Test: declare knowledge, rebuild indexes, assemble_context, assert layers
populated and total_tokens ≤ budget.

- [ ] **Step 4: Verify**

```bash
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m2): assemble_context packing policy"
```

---

## Task 13: Index rebuild determinism + extended reproject

**Files:**
- Modify: `crates/oxibrain-store/src/reproject.rs`
- Create: `crates/oxibrain/tests/m2_index_determinism.rs`

**Interfaces:**
- Produces: extended `reproject` that rebuilds indexes after replay.

- [ ] **Step 1: Extend reproject.rs**

After the episode replay loop in `reproject()`, add index rebuild for each
space that has declaration episodes:

```rust
    // 4. Rebuild indexes for all spaces.
    let mut space_stmt = conn
        .prepare("SELECT DISTINCT id FROM spaces")
        .map_err(sql_err)?;
    let spaces: Vec<String> = space_stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    drop(space_stmt);
    for space in &spaces {
        crate::index_ops::rebuild_indexes(conn, space)?;
        crate::communities::rebuild_communities(conn, space)?;
    }
```

- [ ] **Step 2: Write determinism test**

`crates/oxibrain/tests/m2_index_determinism.rs`:
```rust
//! Index rebuild determinism: reproject produces byte-identical index tables.

use oxibrain::Brain;
use oxibrain_core::retrieval::*;
use oxibrain_ports::*;
use oxibrain_store::project::*;
use tempfile::tempdir;

#[tokio::test]
async fn index_rebuild_is_deterministic() {
    let dir = tempdir().expect("tempdir");
    let clock = std::sync::Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(
        oxibrain::BrainConfig::at(dir.path().to_str().unwrap()),
        clock,
    )
    .await
    .expect("open");

    let space = brain.ensure_space("test").await.expect("space");
    // Declare some knowledge...
    brain
        .declare(&space, Declaration::AddStatement {
            subject: EntityRef { surface: "Alice".into(), ty: "Person".into() },
            predicate: "works_on".into(),
            object: DeclObject::Entity { surface: "ProjectX".into(), ty: "Project".into() },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        })
        .await
        .expect("declare");

    // Snapshot index tables.
    let snapshot1 = brain.snapshot_indexes(&space).await.expect("snapshot");

    // Reproject.
    brain.reproject().await.expect("reproject");

    // Snapshot again.
    let snapshot2 = brain.snapshot_indexes(&space).await.expect("snapshot");

    assert_eq!(snapshot1, snapshot2, "index tables must be byte-identical after reproject");
}
```

Add a **store-level** `snapshot_indexes` function (avoids rusqlite in the facade):

In `crates/oxibrain-store/src/index_ops.rs`:
```rust
/// Snapshot index tables into a comparable string for determinism tests.
pub fn snapshot_indexes(conn: &Connection, space: &str) -> Result<String, BrainError> {
    let mut out = String::new();
    for (label, sql) in [
        ("fts", "SELECT target_kind, target_id, body FROM episodes_fts WHERE space_id = ?1 ORDER BY target_kind, target_id"),
        ("vec", "SELECT target_kind, target_id, hex(vector) FROM tfidf_vectors WHERE space_id = ?1 ORDER BY target_kind, target_id"),
        ("com", "SELECT id, label FROM communities WHERE space_id = ?1 ORDER BY id"),
    ] {
        let mut stmt = conn.prepare(sql).map_err(sql_err)?;
        let rows: Vec<String> = stmt
            .query_map(params![space], |r| {
                let n = r.column_count().unwrap_or(0);
                let mut parts = Vec::with_capacity(n);
                for i in 0..n {
                    parts.push(r.get_ref(i)?.as_str().map(|s| s.to_string()).unwrap_or_default());
                }
                Ok(parts.join("|"))
            })
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        out.push_str(&format!("---{label}---\n"));
        for r in rows {
            out.push_str(&r);
            out.push('\n');
        }
    }
    Ok(out)
}
```

In the facade, the test helper just wraps the store call:
```rust
    pub async fn snapshot_indexes(&self, space: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| oxibrain_store::index_ops::snapshot_indexes(conn, &space))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```
Where `index_ops_handler` is `oxibrain_store::index_ops::snapshot_indexes`.

- [ ] **Step 3: Verify**

```bash
cargo test -p oxibrain --test m2_index_determinism
cargo test -p oxibrain --test reproject_determinism   # M1 test must still pass
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m2): index rebuild determinism + extended reproject"
```

---

## Task 14: Bench suite (criterion)

**Files:**
- Modify: `crates/oxibrain/Cargo.toml` (add `[[bench]]` + criterion dev-dep)
- Create: `crates/oxibrain/benches/budget.rs`

**Interfaces:**
- Produces: criterion bench suite measuring all §13.2 budgets.

- [ ] **Step 1: Add bench config to Cargo.toml**

In `crates/oxibrain/Cargo.toml`, add:
```toml
[[bench]]
name = "budget"
harness = false

[dev-dependencies]
criterion.workspace = true
```

(Merge with existing `[dev-dependencies]` — add criterion to the existing section.)

- [ ] **Step 2: Write budget.rs bench**

`crates/oxibrain/benches/budget.rs`:
```rust
//! §13.2 performance budget benchmarks.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use oxibrain::Brain;
use oxibrain_core::retrieval::{Query, QueryMode, TraversalSpec};
use oxibrain_ports::{FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use std::sync::Arc;

fn build_fixture(dir: &std::path::Path) -> Brain {
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
        let brain = Brain::with_clock(
            oxibrain::BrainConfig::at(dir.to_str().unwrap()),
            clock,
        )
        .await
        .expect("open");
        let space = brain.ensure_space("bench").await.expect("space");

        // Declare 200 entities, 500 statements in a graph pattern.
        let mut i = 0;
        while i < 200 {
            let subj = format!("Entity{i}");
            let pred = if i % 3 == 0 { "works_on" } else { "knows" };
            let obj = format!("Entity{}", (i + 1) % 200);
            brain
                .declare(&space, Declaration::AddStatement {
                    subject: EntityRef { surface: subj.clone(), ty: "Concept".into() },
                    predicate: pred.into(),
                    object: DeclObject::Entity { surface: obj.clone(), ty: "Concept".into() },
                    polarity: "affirm".into(),
                    valid_from: oxibrain_ports::TIME_MIN.millis(),
                    valid_to: oxibrain_ports::TIME_MAX.millis(),
                })
                .await
                .expect("declare");
            i += 1;
        }
        brain.rebuild_indexes(&space).await.expect("rebuild");
        brain
    })
}

fn bench_declaration_write(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path());
    let space = tokio::runtime::Runtime::new().unwrap().block_on(brain.ensure_space("bench"));
    c.bench_function("declaration_write", |b| {
        b.iter(|| {
            let brain = &brain;
            let space = &space;
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                brain.declare(space, Declaration::AddStatement {
                    subject: EntityRef { surface: "BenchEntity".into(), ty: "Concept".into() },
                    predicate: "knows".into(),
                    object: DeclObject::Entity { surface: "BenchTarget".into(), ty: "Concept".into() },
                    polarity: "affirm".into(),
                    valid_from: oxibrain_ports::TIME_MIN.millis(),
                    valid_to: oxibrain_ports::TIME_MAX.millis(),
                }).await.expect("declare");
            });
        });
    });
}

fn bench_hybrid_query(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path());
    let space = tokio::runtime::Runtime::new().unwrap().block_on(brain.ensure_space("bench"));
    c.bench_function("hybrid_query_top20", |b| {
        b.iter(|| {
            let brain = &brain;
            let space = &space;
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                brain.query(Query {
                    text: "Entity50".into(),
                    mode: QueryMode::Hybrid,
                    space: space.clone(),
                    as_of: None,
                    limit: 20,
                    min_confidence: 0.0,
                }).await.expect("query");
            });
        });
    });
}

fn bench_traversal(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path());
    let space = tokio::runtime::Runtime::new().unwrap().block_on(brain.ensure_space("bench"));
    c.bench_function("traversal_depth3_256", |b| {
        b.iter(|| {
            let brain = &brain;
            let space = &space;
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                brain.traverse(space, TraversalSpec {
                    start: vec!["test_entity".into()],
                    ..Default::default()
                }).await.ok();
            });
        });
    });
}

criterion_group!(benches, bench_declaration_write, bench_hybrid_query, bench_traversal);
criterion_main!(benches);
```

- [ ] **Step 3: Verify benches compile**

```bash
cargo bench --no-run
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m2): criterion bench suite for §13.2 budgets"
```

---

## Self-Review Notes

After all 14 tasks, verify:

1. **Standalone guarantee:**
```bash
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1   # no match
```

2. **Full gates:**
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

3. **M1 regression:** `reproject_is_byte_identical` still passes.
4. **M2 exit:** benches run; multi-hop + thematic queries answer over a hand-built graph.
