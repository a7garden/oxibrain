# Daemonless Two-Plane Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `docs/superpowers/specs/2026-08-27-two-plane-documents-and-memory-design.md` — separate `documents.db` cache plane, gix read-side, operation-scoped locks, daemon removal, queue-less extraction, clean CLI/MCP/client cutover.

**Architecture:** Memory stays in `brain.db` (ledger + projections). Documents live only in a new disposable `documents.db` cache reconciled from configured roots + gix at query time. `Brain` becomes a handle-free runtime facade; every method opens its store for the duration of one operation. The daemon, watcher, socket discovery, sync, and extraction queue are deleted; callers use the CLI, a caller-owned `serve --stdio` child, or foreground HTTP.

**Tech Stack:** Rust 2024, rusqlite (bundled) + sqlite-vec, gix 0.83 (read-only), blake3, tokio, clap 4, proptest.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-27-two-plane-documents-and-memory-design.md` is authoritative; naming change: the plane selector is **`SearchPlane`** (the spec's `SearchTarget` name collides with the existing `oxibrain_core::retrieval::SearchTarget` hit-target enum).
- `oxibrain-core` must not name rusqlite/tokio; `oxibrain-store` is the only crate that opens SQLite connections (including `documents.db`); `oxibrain-connectors` does filesystem + gix only; core decides, store fetches/writes, facade sequences (P9).
- No `oxios-*` / `oxicode-*` dependency anywhere; `gix` is allowed (not an oxi crate).
- MCP tool cap stays **fifteen**; `document_history` is a native JSON-RPC method, not a new MCP tool.
- clippy clean with `-D warnings`; `#![cfg_attr(test, allow(clippy::unwrap_used))]`; no bare `unwrap` outside tests; English comments/commits.
- No model/network/embedding call inside any DB transaction.
- Version bumps in the final task: workspace `0.6.0 → 0.7.0`, `oxibrain-client` `0.7.0 → 0.8.0`.
- Every task ends with `cargo test -p <crate>` green for that crate plus `cargo clippy -p <crate> --all-targets -- -D warnings`, then commits.

## File Structure (frozen interfaces)

New files:
- `crates/oxibrain-core/src/documents.rs` — pure planners + observation types.
- `crates/oxibrain-store/src/documents.rs` — `documents.db` open/apply/query.
- `crates/oxibrain-store/src/migrations/v11.sql` — drop `ingest_jobs`.
- `crates/oxibrain-connectors/src/documents_config.rs` — `documents.toml`.
- `crates/oxibrain-connectors/src/scan.rs` — plain-root scanner.
- `crates/oxibrain-connectors/src/git_docs.rs` — read-only gix reader.
- `crates/oxibrain-connectors/src/decode.rs` — versioned decoders.
- `crates/oxibrain/src/document_plane.rs` — facade-side reconcile/materialize/history.
- `crates/oxibrain-cli/src/cmd/index.rs` — `oxibrain index [--documents] [--embed]`.

Deleted files (final-surface task): `crates/oxibrain/src/vault.rs`, `crates/oxibrain-cli/src/cmd/sync.rs`, `crates/oxibrain-mcp/src/daemon.rs`, `crates/oxibrain-client/src/discovery.rs`, `crates/oxibrain-connectors/src/watch.rs`.

---

### Task 1: Core — pure document planners and plane types

**Files:**
- Create: `crates/oxibrain-core/src/documents.rs`
- Modify: `crates/oxibrain-core/src/lib.rs` (add `pub mod documents;`)
- Modify: `crates/oxibrain-core/src/retrieval.rs` (add `SearchPlane`, `Query.planes`)
- Modify: `crates/oxibrain-core/src/pack.rs` (`ContextInput.documents`, `DocumentExcerpt`)
- Modify: `crates/oxibrain-core/src/context.rs` (`ContextLayer::Documents` position)
- Test: `crates/oxibrain-core/tests/documents_planners.rs`

**Interfaces (produced — later tasks import exactly these):**

```rust
// documents.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileObservation {
    pub locator: String,          // root-relative, '/' separators
    pub bytes: u64,
    pub modified_ns: i64,
    pub revision_hint: Option<String>, // Some(git blob oid) for clean tracked files
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFile {
    pub locator: String,
    pub bytes: u64,
    pub modified_ns: i64,
    pub revision: String,
}

/// Config-fingerprint inputs for one root. Equality decides Keep vs Reset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootFingerprint {
    pub alias: String,
    pub canonical_path: String,
    pub space: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedRootMeta {
    pub alias: String,
    pub space: String,
    pub fingerprint: RootFingerprint,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootAction { KeepRoot, ResetRoot, RemoveRoot }

/// Pure root-set diff. Deterministic: output sorted by alias.
pub fn diff_roots(configured: &[RootFingerprint], cached: &[CachedRootMeta]) -> Vec<(String, RootAction)>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FileAction {
    Unchanged,
    Add(FileObservation),
    Replace(FileObservation),
    Delete { locator: String },
    Skip { locator: String, reason: String },
}

/// Pure per-root reconcile. Every cached and observed locator lands in exactly
/// one action; output sorted by locator.
pub fn plan_reconcile(cached: &[CachedFile], observed: &[FileObservation]) -> Vec<FileAction>;

/// blake3 hex of (document_id, revision, ordinal) — chunk id derivation used
/// by store and facade. document_id = blake3 hex of (root_alias, locator).
pub fn document_id(root_alias: &str, locator: &str) -> String;
pub fn chunk_id(document_id: &str, revision: &str, ordinal: u32) -> String;
```

```rust
// retrieval.rs additions
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchPlane { Memory, Documents }
// Query gains: #[serde(default = "default_planes")] pub planes: BTreeSet<SearchPlane>,
//              fn default_planes() -> BTreeSet<SearchPlane> { both }
```

```rust
// pack.rs additions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentExcerpt {
    pub uri: String,   // doc://alias/<pct-locator>?rev=<revision>
    pub text: String,  // verbatim decoded slice
    pub score: f32,
}
// ContextInput gains: #[serde(default)] pub documents: Vec<DocumentExcerpt>,
```

- `plan_reconcile` semantics: cached∩observed with equal `(bytes, modified_ns, revision-compatible)` → `Unchanged`; equal stat but different `revision_hint` vs cached revision → `Replace`; observed-only → `Add`; cached-only → `Delete`. There is no `Skip` from the pure planner (skip decisions are I/O-level: unreadable/oversized — connector marks them by omitting from `observed` and adding to a `skipped` list it returns separately; `Skip` remains in the enum for the apply-stage report).

- [ ] **Step 1: Write failing property/unit tests** — conservation (every cached and observed locator appears exactly once across actions), determinism (sorted by locator), diff_roots keep/reset/remove table, `document_id`/`chunk_id` stability (fixed vectors), revision-hint change ⇒ Replace. Include proptest for conservation over random inputs.
- [ ] **Step 2: `cargo test -p oxibrain-core` → fail** (module missing).
- [ ] **Step 3: Implement**; wire `ContextLayer::Documents` between `QueryNeighborhood` and `RecentEpisodes` in layer ordering and give it a `reserve` floor equal to `QueryNeighborhood`'s minimum.
- [ ] **Step 4: `cargo test -p oxibrain-core` and `cargo clippy -p oxibrain-core --all-targets -- -D warnings` → green.**
- [ ] **Step 5: Commit** `feat(core): pure document planners, search planes, context document layer`

---

### Task 2: Store — v11 migration, queue removal, export/import, uncached query

**Files:**
- Create: `crates/oxibrain-store/src/migrations/v11.sql`
- Modify: `crates/oxibrain-store/src/schema.rs` (`LEDGER_SCHEMA_VERSION = 11`)
- Modify: `crates/oxibrain-store/src/extraction.rs` — delete `enqueue_job`, `claim_jobs`, `complete_job`, `fail_job`, `reclaim_expired`, `list_jobs`, `IngestJob`, `JobState`; delete their tests; replace `uncached_episodes` with:

```rust
/// Eligible memory-plane backlog: primary, non-document, not redacted,
/// no extraction row for this extractor, ordered by seq.
pub fn uncached_memory_episodes(conn: &Connection, space: &str, extractor_id: &str) -> Result<Vec<String>, BrainError>;
// WHERE e.kind = 'primary'
//   AND e.source_kind NOT IN ('document', 'document_revision')
//   AND e.redacted_at IS NULL
//   AND NOT EXISTS (SELECT 1 FROM extractions x WHERE x.episode_id = e.id AND x.extractor_id = ?2)
```

- Modify: `crates/oxibrain-store/src/export.rs` — remove `ingest_jobs` from `EXPORT_TABLES`; in import, a line whose table is exactly `ingest_jobs` is skipped with `tracing::warn!` and counted in the return summary; any other unknown table remains an error (verify current behavior and keep it).
- Modify: `crates/oxibrain-store/src/lib.rs` / `extraction.rs` tests: job tests removed; add migration up-test (build a store at v10 using existing migration-test helpers — check `migration.rs` for a fixture/chain test to extend — asserting v11 applied, `ingest_jobs` gone, episodes/sources rows untouched and FK-valid, meta key `v11_ingest_jobs_dropped` = pre-drop count).
- v11.sql body:

```sql
-- v11: queue-less extraction (two-plane design §11.1). The backlog is the
-- uncached_memory_episodes query; the durable queue is retired.
CREATE TABLE IF NOT EXISTS _v11_jobs AS SELECT COUNT(*) AS n FROM ingest_jobs;
INSERT OR REPLACE INTO meta (key, value)
  SELECT 'v11_ingest_jobs_dropped', CAST(n AS TEXT) FROM _v11_jobs;
DROP TABLE IF EXISTS _v11_jobs;
DROP TABLE IF EXISTS ingest_jobs;
```

(Adapt to the actual `meta` schema used by migrations — check `meta.rs`; if `meta` has different columns, insert via the Rust step instead. The Rust migration step may record the count more cleanly than SQL; prefer whichever matches existing migration style.)

- [ ] **Step 1: Failing tests** — v10→v11 up-test (FK intact, table dropped, count recorded); import pre-v11 JSONL containing an `ingest_jobs` line succeeds and skips it; import with unknown table `nope` errors; `uncached_memory_episodes` excludes `document`/`document_revision` kinds and redacted rows.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: `cargo test -p oxibrain-store`, clippy → green.**
- [ ] **Step 5: Commit** `feat(store): schema v11 — retire ingest_jobs, memory-plane backlog query`

---

### Task 3: Store — documents.db cache

**Files:**
- Create: `crates/oxibrain-store/src/documents.rs`
- Modify: `crates/oxibrain-store/src/lib.rs` (`pub mod documents;`)
- Test: `crates/oxibrain-store/tests/documents_cache.rs`

**Interfaces (produced):**

```rust
use oxibrain_core::documents::{CachedFile, CachedRootMeta, FileAction, RootAction, RootFingerprint};

pub struct DocumentCache { /* connection + lock guard */ }

impl DocumentCache {
    /// Open read-write with an exclusive advisory lock on `<dir>/documents.lock`.
    /// Creates + migrates documents.db (schema version 1). Lock refusal => BrainError::Busy.
    pub fn open_rw(dir: &Path) -> Result<Self, BrainError>;
    /// Read-only WAL reader, no lock.
    pub fn open_ro(dir: &Path) -> Result<Self, BrainError>;

    pub fn list_roots(&self) -> Result<Vec<CachedRootMeta>, BrainError>;
    pub fn root_manifest(&self, alias: &str) -> Result<Vec<CachedFile>, BrainError>;
    pub fn generation(&self, alias: &str) -> Result<i64, BrainError>;

    /// One atomic apply. `expected_generation` per root gives CAS: mismatch => Err(BrainError::Busy).
    pub fn apply(&self, plan: &ApplyPlan) -> Result<(), BrainError>;
}

pub struct ApplyPlan {
    pub root_actions: Vec<(String, RootAction)>,
    /// Per kept/reset root, after root-level reset is applied:
    pub roots: Vec<RootApply>,
}
pub struct RootApply {
    pub fingerprint: RootFingerprint,
    pub expected_generation: i64,
    pub actions: Vec<FileAction>,
    /// Ready payloads for Add/Replace: decoded-text chunks + manifest row.
    pub upserts: Vec<DocumentUpsert>,
}
pub struct DocumentUpsert {
    pub locator: String,
    pub revision: String,
    pub media_type: String,       // "text/markdown" | "text/html" | "text/plain"
    pub bytes: u64,               // raw file bytes
    pub modified_ns: i64,
    pub modified_at: i64,         // seconds for display
    pub chunks: Vec<ChunkUpsert>, // final chunk set for this document
}
pub struct ChunkUpsert {
    pub ordinal: u32,
    pub span_start: usize, // byte offsets into decoded text
    pub span_end: usize,
    pub context: String,   // render_context_prefix output
    pub text: String,      // decoded slice, for FTS + embedding only
}

pub struct CachedChunk {
    pub chunk_id: String,
    pub document_id: String,
    pub root_alias: String,
    pub space: String,
    pub locator: String,
    pub revision: String,
    pub media_type: String,
    pub ordinal: u32,
    pub span_start: usize,
    pub span_end: usize,
    pub modified_at: i64,
}

impl DocumentCache {
    /// Lexical search over doc_fts_word / doc_fts_ngram; returns (chunk_id, bm25 score desc).
    pub fn search_fts(&self, space: &str, table: FtsTable, query: &str, limit: usize) -> Result<Vec<(String, f64)>, BrainError>;
    pub fn knn(&self, query_vector: &[f32], limit: usize) -> Result<Vec<(String, f64)>, BrainError>;
    pub fn chunks(&self, ids: &[&str]) -> Result<Vec<CachedChunk>, BrainError>;
    pub fn embedded_count(&self, space: &str) -> Result<(u64, u64), BrainError>; // (embedded, total)
    pub fn pending_vector_chunks(&self, space: &str, limit: usize) -> Result<Vec<(String, String)>, BrainError>; // (chunk_id, text)
    pub fn upsert_vectors(&self, rows: &[(String, Vec<f32>)]) -> Result<(), BrainError>;
    /// Called when embedding model identity changes: DELETE FROM doc_vectors.
    pub fn clear_vectors(&self) -> Result<(), BrainError>;
    pub fn meta_get(&self, key: &str) -> Result<Option<String>, BrainError>;
    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), BrainError>;
}
```

Schema (documents.db v1) — from spec §6 verbatim, plus `media_type` on `documents`. Apply semantics in one transaction:

1. `RemoveRoot`/`ResetRoot`: for each affected alias — `DELETE FROM doc_vectors WHERE chunk_id IN (SELECT id FROM doc_chunks JOIN documents ON document_id = documents.id WHERE root_alias = ?)`, delete FTS rows by chunk_id join, cascade deletes documents/chunks/manifest/root row; `ResetRoot` then re-inserts the root row with generation 1.
2. Per `RootApply`: CAS on generation (`SELECT generation FROM doc_roots WHERE alias = ?` equals `expected_generation`, else `Busy`); apply `Delete` (vector+FTS+rows), `Add`/`Replace` (delete old vector/FTS rows for that document, upsert documents row, insert chunks, insert FTS rows for both tokenizers with chunk text, upsert manifest); `Unchanged` no-op; bump generation; write `config_hash` = blake3 of fingerprint fields.
3. Failure rolls the whole transaction back.

FTS query building reuses the quoting approach from `query.rs:458-463` (quoted tokens, implicit AND). vec0 KNN mirrors existing `vectors` usage in `vectors.rs`.

- [ ] **Step 1: Failing tests** — open creates schema; apply add/replace/delete leaves exactly expected rows in documents/chunks/both FTS tables/vectors (vectors empty until upserted; replacement removes old FTS+vector rows); remove-root cascade clears everything; CAS mismatch ⇒ Busy and no partial state; rebuild equivalence: apply same observations to a fresh cache ⇒ identical documents/chunks/FTS membership; `embedded_count`, `pending_vector_chunks`, `clear_vectors` behavior.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: `cargo test -p oxibrain-store`, clippy → green.**
- [ ] **Step 5: Commit** `feat(store): documents.db cache — schema, atomic apply, FTS/vector reads`

---

### Task 4: Connectors — config, scanner, decoders

**Files:**
- Create: `crates/oxibrain-connectors/src/documents_config.rs`, `scan.rs`, `decode.rs`
- Modify: `crates/oxibrain-connectors/src/lib.rs`, `crates/oxibrain-connectors/Cargo.toml` (add `glob = "0.3"`, `blake3.workspace = true`, `hex.workspace = true`, `toml = "0.8"`)
- Test: `crates/oxibrain-connectors/tests/documents_config.rs`, `tests/scan_decode.rs`

**Interfaces (produced):**

```rust
// documents_config.rs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RootEntry {
    pub alias: String,
    pub path: PathBuf,          // ~ expanded at load
    pub space: String,
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
}
pub struct DocumentsConfig { pub roots: Vec<RootEntry> }

impl DocumentsConfig {
    pub fn load(dir: &Path) -> Result<DocumentsConfig, ConfigError>;   // dir/documents.toml; missing file => empty (not an error)
    pub fn save(dir: &Path, cfg: &DocumentsConfig) -> Result<(), ConfigError>;
    pub fn validate(&self) -> Result<(), ConfigError>;                 // unique aliases; non-empty alias/space/path
    pub fn root(&self, alias: &str) -> Option<&RootEntry>;
}
#[derive(Debug, thiserror::Error)] pub enum ConfigError { /* Parse(String), Invalid(String), Io(String) */ }
```

```rust
// scan.rs
pub struct SkippedFile { pub locator: String, pub reason: String }
pub struct ScanResult {
    pub observations: Vec<FileObservation>,   // sorted by locator
    pub skipped: Vec<SkippedFile>,
}
/// Walk a root: no symlink follow, files only, include/exclude globs,
/// oversize/unreadable-metadata => skipped. revision_hint left None here.
pub fn scan_root(root: &RootEntry) -> Result<ScanResult, BrainError>;
/// Canonicalize without following a symlinked final component beyond config rules:
/// reject if the canonical path escapes the canonical root.
pub fn canonicalize_root(path: &Path) -> Result<PathBuf, BrainError>;
```

```rust
// decode.rs
pub const DECODER_VERSION: &str = "1";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType { Markdown, Html, PlainText }
impl MediaType { pub fn from_extension(ext: &str) -> Option<MediaType>; pub fn as_str(&self) -> &'static str; }
pub struct DecodedDocument { pub media_type: MediaType, pub text: String } // text is valid UTF-8
/// Deterministic decode: markdown frontmatter stripped (reuse markdown.rs logic),
/// html -> text (reuse html.rs), plain passthrough with lossy-UTF8 replacement.
pub fn decode(media_type: MediaType, bytes: &[u8]) -> Result<DecodedDocument, BrainError>;
```

`FileObservation` comes from `oxibrain-core::documents` (add `oxibrain-core.workspace = true` dep to connectors — allowed; core has no I/O). `modified_ns`: `metadata.modified()` → nanos since epoch as i64 (clamp negative to 0).

- [ ] **Step 1: Failing tests** — config round-trip (TOML in the spec's exact shape), duplicate alias rejected, missing file ⇒ empty config, `~` expansion; scanner: nested files, excluded patterns, oversize skip, symlink not followed, sorted deterministic output, in-place edit changes `modified_ns` observation; decoder: frontmatter stripped md, html→text deterministic, plain lossy, unknown extension ⇒ None.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: `cargo test -p oxibrain-connectors`, clippy → green.**
- [ ] **Step 5: Commit** `feat(connectors): documents.toml config, root scanner, versioned decoders`

---

### Task 5: Connectors — read-only gix document reader

**Files:**
- Create: `crates/oxibrain-connectors/src/git_docs.rs`
- Modify: `crates/oxibrain-connectors/src/lib.rs`, `Cargo.toml` (add `gix = { workspace = true }`; workspace `Cargo.toml` gains `gix = "0.83"` — no oxi deps, standalone preserved)
- Test: `crates/oxibrain-connectors/tests/git_docs.rs`

**Interfaces (produced):**

```rust
pub struct GitDocumentReader { repo: gix::Repository }

pub struct GitBlob { pub oid: String, pub bytes: u64 }
pub struct GitSnapshot {
    pub head: Option<String>,                  // hex object id of HEAD commit
    pub object_format: String,                 // "sha1"
    pub tracked: std::collections::BTreeMap<String, GitBlob>, // locator -> blob
}
pub struct DocumentRevision {
    pub root_alias: String,
    pub locator: String,
    pub revision: String,        // git:<format>:<oid>
    pub content: Vec<u8>,
    pub committed_at: i64,       // commit time, seconds
}

impl GitDocumentReader {
    /// Ok(None) when the path is not inside a git worktree. Never writes.
    pub fn open(root: &Path) -> Result<Option<Self>, BrainError>;
    pub fn snapshot(&self) -> Result<GitSnapshot, BrainError>;
    /// Repository ignore rules for a locator (root + nested .gitignore via gix).
    pub fn is_ignored(&self, locator: &str) -> Result<bool, BrainError>;
    pub fn blob_bytes(&self, oid: &str) -> Result<Vec<u8>, BrainError>;
    /// Revision for current worktree bytes: `git:<format>:<oid>` when equal to
    /// the tracked blob, else `blake3:<hex>`.
    pub fn current_revision(&self, snapshot: &GitSnapshot, locator: &str, worktree_bytes: &[u8]) -> Result<String, BrainError>;
    /// Commit history for a locator, oldest first; entries only where the path exists in the tree.
    pub fn history(&self, root_alias: &str, locator: &str, limit: usize) -> Result<Vec<DocumentRevision>, BrainError>;
    /// Best-effort rename hint: latest commit where old_locator disappeared and a
    /// new path with identical blob oid appeared. None on ambiguity.
    pub fn rename_hint(&self, old_locator: &str) -> Result<Option<String>, BrainError>;
}
```

Critical gix 0.83 guidance for the implementer: open via `gix::open` (fail ⇒ not a repo ⇒ `Ok(None)`); HEAD tree walk via `repo.head().peel_to_commit()?.tree()` and recursive tree descent (follow the `oxi-vault-git` `find_blob_in_tree` pattern, but read-only); commit walk via `repo.rev_walk().single(head)` pushing commit ids, reading `commit_tree`/`committer().time.seconds`; ignore via `repo.excludes` / `gix::exclude` stack matching per path (`repo.path_to_resource` not needed — build Platform with `repo.excludes(None)` and `platform.matching(canonicalized relative path, None)`; adapt to the actual 0.83 API, keep a fallback that honors root `.gitignore` patterns with the `glob` crate and log a warning if the gix exclude path can't be constructed). The vault on this machine is an index-less gix repo — never touch the index; never lock `.git/config` (open read-only: `gix::open_opts(root, gix::open::Options::default().open_path_as_is(true))` if needed to avoid writes; verify with a read-only-directory test).

- [ ] **Step 1: Failing tests** (tempdir repos created **via gix directly with an empty index** — write blob/tree/commit like `oxi-vault-git::create_initial_commit`, no `git add`): snapshot lists HEAD files; history returns oldest-first revisions incl. content; dirty worktree file ⇒ `current_revision` = `blake3:`; clean ⇒ `git:`; untracked ignored-file excluded by `is_ignored`; `rename_hint` finds a rename, ambiguous rename ⇒ None; read-only `.git` directory still opens (no writes); non-repo ⇒ `Ok(None)`.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: `cargo test -p oxibrain-connectors`, clippy → green. Also run `cargo build -p oxibrain --no-default-features` to confirm no oxi deps entered.**
- [ ] **Step 5: Commit** `feat(connectors): read-only gix document reader — HEAD snapshot, history, ignores`

---

### Task 6: Facade — handle-free Brain, document plane, inline extraction

**Files:**
- Create: `crates/oxibrain/src/document_plane.rs`
- Modify: `crates/oxibrain/src/lib.rs` (rework `Brain`), `crates/oxibrain/src/ingest.rs`, `crates/oxibrain/src/extraction.rs`
- Delete: `crates/oxibrain/src/vault.rs`
- Test: `crates/oxibrain/tests/document_plane.rs`, extend existing facade tests

**Interfaces (produced — CLI/MCP consume these):**

```rust
// lib.rs — Brain rework
pub struct Brain {
    config: BrainConfig,
    clock: Arc<dyn ClockPort>,
    llm: Option<Arc<dyn LlmPort>>,
    tokenizer: Arc<dyn TokenizerPort>,
    embedder: Option<Arc<dyn EmbeddingPort>>,
}
// Brain::open(config): validates dir (creating + migrating via one short locked open when absent),
// loads nothing persistent. Clone = cheap field clone. Constructors with_llm*/with_clock* keep names.
// Internal helpers (private):
//   async fn read<T>(&self, f: impl FnOnce(&StoreHandle) -> Result<T>) -> Result<T>        // open_ro per call
//   async fn write<T>(&self, f: impl FnOnce(&mut /* via StoreHandle writer */) -> Result<T>) // locked open + bounded retry
//     retry: [25, 50, 100, 200, 400, 800] ms then BrainError::Locked.

// Public search surface (replaces facade search entry points):
pub struct SearchResponse {
    pub memory: Vec<SearchResult>,        // existing SearchResult shape
    pub documents: Vec<DocumentHit>,
    pub freshness: DocumentFreshness,
}
pub struct DocumentHit {
    pub document_id: String, pub root: String /*alias*/, pub locator: String,
    pub revision: String, pub ordinal: u32, pub text: String,
    pub modified_at: Timestamp, pub score: f64,
}
pub struct DocumentFreshness {
    pub reconciled_roots: Vec<String>,
    pub skipped_roots: Vec<(String, String)>,   // alias, reason (missing etc.)
    pub skipped_files: usize,
    pub stale_after_retry: Vec<String>,         // locators that changed twice
    pub dense_coverage: Option<f64>,            // None when no vector channel ran
}
impl Brain {
    pub async fn search(&self, q: Query) -> Result<SearchResponse, BrainError>;      // planes-aware; space-scoped
    pub async fn recall(&self, space: &str, query: &str, budget: usize, hints: Option<&RecallHints>) -> Result<ContextResult, BrainError>; // gains Documents layer
    pub async fn index_documents(&self, opts: IndexOptions) -> Result<DocumentFreshness, BrainError>;
    pub struct IndexOptions { pub embed: bool, pub budget: Option<usize> }
    pub async fn embed_pending_documents(&self, budget: usize) -> Result<usize, BrainError>;
    pub async fn document_history(&self, space: &str, alias: &str, locator: &str, limit: usize) -> Result<Vec<DocumentRevision>, BrainError>;
    pub async fn pending_extraction_stats(&self) -> Result<PendingStats, BrainError>; // {count, oldest_seq}
    pub async fn extract_uncached(&self, limit: usize) -> Result<usize, BrainError>;
}
// Removed: episodes_for_locator, job_status, extract_pending (lease loop), sync helpers, vault module.
```

`document_plane.rs` (the sequencer, P9):

```
reconcile(dir, config, cache_rw):
  roots_cfg = DocumentsConfig::load(dir)
  cached    = cache_rw.list_roots()
  actions   = core::documents::diff_roots(fingerprints(roots_cfg), cached)
  for each Keep/Reset root:
      scan = scan_root(entry)                      // plain manifest walk
      git  = GitDocumentReader::open(canonical)    // Ok(None) for plain roots
      for each observation:
          if git && git.is_ignored(locator) => move to skipped
          revision = git.current_revision(snapshot, locator, bytes_of_changed_only)
                   // bytes read ONLY for Add/Replace candidates; hint reused for clean tracked
      plan = core::documents::plan_reconcile(cache_rw.root_manifest(alias), observations)
      payloads: for Add/Replace — read bytes (stat before+after; retry once), decode,
                split_into_chunks (existing core chunking policy), render_context_prefix,
                DocumentUpsert
  cache_rw.apply(ApplyPlan { root_actions, roots }) // one tx; Busy => rescan once
materialize_hit(cache_ro, config, chunk):
  entry = config.root(chunk.root_alias)?            // alias gone => drop hit
  bytes = read file; stat check; revision verify vs chunk.revision
        => mismatch: reconcile that root once, refetch chunk, verify again; second fail => stale list
  decoded = decode(media_type, bytes); slice span; verify UTF-8 boundaries
search_documents(space, query, mode):
  reconcile → fts_word + fts_ngram (+ knn when embedder && dense/hybrid mode)
  → RRF k=60 within plane → TargetId::Chunk into existing rank pipeline (conservation)
  → materialize top hits
```

LLM/embedder are loaded per process (existing config/llm wiring in CLI); no DB handle is held across any model call. `remember`/capture/ingest rework in `ingest.rs`/`extraction.rs`: append episode (write op) → extract outside transaction (llm) → validate → write assertions+fold (write op); any failure returns `CapturedPending { episode_id, pending_count }`; `extract_uncached` loops `uncached_memory_episodes` under `limit`.

Legacy exclusion: memory search paths and recent-episode context add `AND source_kind NOT IN ('document','document_revision')` (context.rs recent query + query.rs channels; check `rebuild_fts` episode indexing stays — legacy episodes remain lexically searchable only via explicit legacy operator path: keep FTS rows, exclude from default `search`/`recall` by filtering target episodes at fetch).

- [ ] **Step 1: Failing tests** — end-to-end on tempdir: config with plain root + file; `search(planes=documents)` returns verbatim slice with revision; edit file in place (no root mtime change) ⇒ next search returns new text (invariant 4); delete file ⇒ no hit, no rows (7); remove root from config ⇒ rows gone (9); git root: gix-commit a file (index-less), dirty variant, `document_history` oldest-first; materialization race: swap file between two searches with forced stale cache ⇒ no mismatched text (12); concurrent writers: spawn two `index_documents` on same dir ⇒ both succeed serially (CAS rescan path); `remember` with failing LLM ⇒ CapturedPending + `extract_uncached` recovers; legacy document episode excluded from memory search/recall; `pending_extraction_stats` counts.
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement** (this is the largest task; keep `document_plane.rs` focused on sequencing, push all SQL into store, all decisions into core).
- [ ] **Step 4: `cargo test -p oxibrain`, clippy → green.**
- [ ] **Step 5: Commit** `feat(brain): handle-free facade, document plane, inline extraction`

---

### Task 7: CLI + MCP + client cutover

**Files:**
- Modify: `crates/oxibrain-cli/src/cli.rs`, `main.rs`, `cmd/serve.rs`, `cmd/extract.rs`, `cmd/doctor.rs`, `cmd/stats.rs`, `cmd/init.rs`; create `cmd/index.rs`; delete `cmd/sync.rs`
- Modify: `crates/oxibrain-mcp/src/server.rs`, `lib.rs`, `protocol.rs`; delete `daemon.rs`
- Modify: `crates/oxibrain-client/src/lib.rs`; delete `discovery.rs`
- Modify: both crates' `Cargo.toml` (mcp drops `notify`; client adds `tokio` process features)

**CLI surface:**

```
oxibrain init [--space]              # seeds documents.toml ONLY per spec §4: explicit init, default dir, no --dir passed, ~/.oxi/vault exists
oxibrain index [--documents] [--embed] [--dir D]
oxibrain extract --pending [--limit N] [--dir D]      # replaces extract <episode>
oxibrain serve [--stdio] | --http <addr> [--dir D]    # --socket/--daemon removed
oxibrain doctor [--dir D]           # + freshness, skipped roots/files, dangling doc:// refs, legacy pull sources, pending stats
oxibrain stats [--dir D]            # + pending extraction count/oldest age, document/root counts
# sync removed
```

**MCP/stdio session:** stdio JSON-RPC loop already exists (Claude Desktop path) — keep it as the only socket-free transport; `--http <addr>` foreground console unchanged apart from handle-free Brain. Delete: `--socket`, `--daemon`, `PidFile`, `start_source_watchers`, `sync/run` handler, `episodes/for_locator` handler. Add native JSON-RPC method `document_history` `{space, alias, locator, limit}` → array of `{revision, committed_at, content}` (read-gated like `resources/read`). MCP `search` tool: gains optional `planes` parameter (default both) and returns `{memory: [...], documents: [...], freshness: {...}}` — still **one tool**, cap intact.

**Client:** delete `discovery.rs`, `connect_default`, `connect_endpoint`, `default_socket_path`; keep `connect(path)` only if some caller needs sockets — remove it (clean cutover: no socket support at all). Add:

```rust
pub struct LocalProcessEndpoint { pub executable: PathBuf, pub dir: PathBuf }
impl BrainClient {
    /// Spawn `executable serve --stdio --dir <dir>`; newline JSON-RPC over piped stdio.
    /// Child dies when the handle drops (kill_on_drop).
    pub async fn spawn_local(endpoint: LocalProcessEndpoint) -> Result<Self>;
    pub async fn spawn_local_with_token(endpoint: LocalProcessEndpoint, token: &str) -> Result<Self>;
    pub async fn document_history(&self, space: &str, alias: &str, locator: &str, limit: usize) -> Result<Vec<DocumentRevisionDto>>;
    // search returns SearchResponseDto { memory, documents, freshness }
    // extract_pending -> extract_uncached(limit), pending_stats()
    // SyncOutcome / sync_run removed
}
```

Stdio framing: the server's existing newline-delimited JSON-RPC session (verify `run_session` framing on stdio matches the client line protocol; adapt client transport from `UnixStream` halves to `Child` piped stdio with the same BufReader/lines logic). Update in-crate tests (`client_round_trip`, `handshake` in mcp, `serve` e2e in cli) to spawn stdio children with temp `--dir`; delete socket-based tests; add: temp-`--dir` child never opens `~/.oxi/brain` or `~/.oxi/vault` (invariant 15 — assert via a `documents.toml` absent + strace-free behavioral check: run with `$HOME` pointed at a temp dir and assert no files created under it).

- [ ] **Step 1: Failing tests** — serve stdio round-trip with spawned child; client `spawn_local` + `document_history` + planes-aware search; `extract --pending` CLI; deleted commands absent from clap (compile-time); init seeding rule (default-dir only, `--dir` never seeds, no `~/.oxi/vault` ⇒ no seed).
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement cutover.**
- [ ] **Step 4: `cargo test -p oxibrain-cli -p oxibrain-mcp -p oxibrain-client`, clippy → green.**
- [ ] **Step 5: Commit** `feat(cli,mcp,client): daemonless cutover — stdio sessions, no socket discovery`

---

### Task 8: Docs, ADRs, version bumps

**Files:**
- Modify: `doc/ARCHITECTURE.md` (§1.3 shape, P1 scope note, P8 op-scoped, §4.2 data flow, §4.3 modes, §5.1 zones + documents.db, §9.1 stages, §15.7 discovery removal, §16 commands; bump version header)
- Modify: `doc/adr/ADR-010` (Status: Superseded by two-plane design), `ADR-011` (amendment note §4.5 → gix read-only history), `ADR-007` (socket/discovery superseded)
- Modify: `doc/CONSUMPTION_CONTRACT.md` (removals/additions per spec §14; version note oxibrain 0.7 / client 0.8)
- Modify: `doc/ECOSYSTEM.md` (§0/§1.2 tables, C4, C8)
- Modify: workspace `Cargo.toml` version `0.7.0`, client `0.8.0` (+ any internal version refs)

- [ ] **Step 1:** Apply doc edits (concise, factual; no stale daemon text remains — grep for `daemon`, `socket`, `sync/run` and clean).
- [ ] **Step 2:** Commit `docs: daemonless two-plane cutover — architecture, ADRs, contracts` + `chore: bump versions to 0.7.0 / client 0.8.0`.

---

### Task 9: Full verification

- [ ] `cargo fmt --all`
- [ ] `cargo build` (whole workspace)
- [ ] `cargo test` (whole workspace)
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo build -p oxibrain --no-default-features --features http-llm`
- [ ] `cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'` → expect no match
- [ ] `cargo tree -p oxibrain-cli | grep -E 'oxios-|oxicode-'` → expect no match
- [ ] Smoke on a tempdir store: `oxibrain init`, write `documents.toml` pointing at a temp root, `index --documents`, `search`, `extract --pending` — via the real binary (`cargo run -p oxibrain-cli --`).
- [ ] Fix anything found; final commit.

## Self-Review (done at plan time)

- Spec §2–§16 mapped to Tasks 1–9 (§4 config → T4/T7-init; §5 gix → T5; §6 cache → T3; §7 reconcile/materialize → T1/T3/T6; §8 retrieval → T1/T6; §9 extraction → T2/T6; §10 removals → T6/T7; §11 migration → T2/T3; §12 failure/security → T4/T5/T6/T7 tests; §13 invariants → per-task tests as listed; §14 docs → T8; §15 operator → smoke in T9).
- Naming collision resolved: `SearchPlane` (plan) replaces spec's `SearchTarget` selector.
- Type consistency: `FileObservation`/`CachedFile`/`RootFingerprint` defined once (T1) and reused verbatim in T3/T4/T6; `DocumentUpsert`/`ChunkUpsert`/`CachedChunk` defined once (T3) and consumed by T6; `DocumentRevision` defined once (T5) and re-exported through facade (T6) and client DTO (T7).
