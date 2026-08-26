# Two Planes, No Daemon: Documents are a Cache, Memory is a Ledger — Design

> **Date:** 2026-08-27 · **Status:** draft, awaiting user review
> **Scope:** Split what oxibrain retrieves from what it believes, remove the
> resident daemon topology, and make every store lock operation-scoped.
> Documents live in a separate rebuildable cache database; explicit memory
> remains in the immutable episode ledger.
> **Supersedes on acceptance:** ADR-010 (daemon-hosted vault watch) and the
> daemon/discovery portions of ADR-007. Amends ADR-011 and the Consumption
> Contract. Requires `ARCHITECTURE.md` §1.3, §3 P8, §4.2–4.3, §5.1, §9.1,
> §15.7, §16, §23; `ECOSYSTEM.md`; and `CONSUMPTION_CONTRACT.md` to change in
> the same implementation cutover.

## 1. Problem

### 1.1 Production evidence

Measured on 2026-08-27 against `~/.oxi/brain/brain.db`:

| Observation | Value |
|---|---|
| `sources` rows | 1024, including 1018 macOS temporary directories |
| `episodes` / `ingest_jobs` / `beliefs` / `entities` | 301 / 216 / 3 / 5 |
| episodes referencing pull sources | 196 |
| vault git | 37 commits and 37 files in the HEAD tree |

The earlier observation that `git ls-files` returned zero was misinterpreted.
The shared `oxi-vault-git` layer intentionally bypasses the Git index and writes
blobs, trees, and commits directly with `gix`. `git ls-files` therefore reports
zero while `git ls-tree -r HEAD` correctly reports 37 committed files. The git
layer works; the index was the wrong instrument.

The source-registry finding remains valid. An oximemo test suite connected to the
well-known production socket and registered throwaway roots. At daemon startup,
the watcher adopted those durable rows and attempted to watch paths that no
longer existed. The ambient socket converted an isolated test into a production
write capability.

The extraction finding also remains valid. The daemon worker left 216 jobs
pending and produced only three beliefs. Daily use did not depend on belief
extraction from vault documents. The durable queue stored an answer that can be
computed as a set difference and then failed silently.

### 1.2 Content diagnosis

The vault contains fiction, article clippings, lyrics, study material, drafts,
scratch notes, and work conventions. Treating those documents as the user's
beliefs is wrong:

- fictional characters can resolve to real people;
- clippings and lyrics become claims the user never made;
- study material becomes personal belief;
- drafts create artificial contradictions;
- convention documents need verbatim text, revision, and path, not a
  fold-selected winner.

The governing rule is:

> **Oxibrain believes only what the user or agent explicitly tells it, and
> retrieves every configured document the user keeps.**

### 1.3 Structural diagnosis

Three mutable operational registries duplicate state owned elsewhere:

| Mutable state | The authoritative question |
|---|---|
| pull `sources` rows | Which roots are in user-owned `documents.toml`? |
| `ingest_jobs` | Which eligible memory episodes lack an extraction? |
| watcher set | Which configured files differ from the cached manifest? |

The correct design asks those questions. It does not persist their answers as
independent authorities.

### 1.4 Existing code that the design reuses

1. Episode lexical indexing is already model-free.
2. `uncached_episodes` already derives an extraction backlog.
3. `Brain::open_ro` already opens WAL readers without the advisory lock.
4. `TargetId::Chunk` and `VecSpace::Chunk` already exist, but have no producer.
5. `oxibrain-core::chunking::{split_into_chunks, render_context_prefix}` is pure
   and deterministic.
6. `oxi-vault-git@0.1.0` is a working gix-based mechanical history layer shared
   by oximemo and oxios. It owns vault writes; oxibrain must not duplicate it.

## 2. Decisions

### 2.1 Two persistence planes

| | Memory plane | Document plane |
|---|---|---|
| Meaning | what oxibrain was explicitly told | what the user keeps |
| Authority | immutable episodes | configured files and git history |
| Database | `brain.db` | `documents.db` |
| Class | irreplaceable ledger plus projections | disposable cache |
| LLM extraction | yes, for eligible primary episodes | never |
| Lexical indexing | episode/statement/entity FTS | chunk FTS |
| Dense indexing | entity vectors | chunk vectors |
| Delete | audited `redact()` | remove file, then reconcile |
| Result | beliefs with uncertainty and sources | verbatim chunk with revision |

`documents.db` is physically separate because a read-only `brain.db` connection
cannot reconcile document cache rows. The split also prevents document content
from reaching the fold by schema construction rather than convention.

### 2.2 No resident daemon

Oxibrain has no launchd service, well-known Unix socket, store-owning background
process, watcher host, or background extraction worker.

Long-lived processes are allowed only when owned by an active caller:

- `oxibrain serve --stdio --dir <dir>` is a child of an MCP or application
  session and exits when stdin closes;
- `oxibrain serve --http --dir <dir>` is an explicitly started foreground
  operations UI and exits when the user stops it.

Neither process owns a database lock for its lifetime. Models may remain loaded;
database handles may not.

### 2.3 One query path, two result lists

`search` and `recall` can target memory, documents, or both. They never compare a
belief score with a document-chunk score. Both targets are restricted to the
caller's already-authorized space.

### 2.4 Git ownership remains with authoring applications

ADR-011 remains authoritative about writes:

- oximemo and oxios use the shared `oxi-vault-git` `GitLayer` to commit, restore,
  and manage history;
- oxibrain opens repositories read-only through `gix` in
  `oxibrain-connectors`;
- oxibrain never initializes, adopts, commits, restores, stages, or modifies a
  repository or vault file;
- the default oxibrain build depends directly on `gix`, not on an `oxi-*` crate,
  preserving the standalone guarantee.

## 3. Runtime topology

### 3.1 Operation-scoped memory handles

`brain.db` retains P8: exactly one writer at a time. What changes is the lifetime
of ownership.

- A read operation calls `Brain::open_ro`, performs the read, and drops it.
- A write operation acquires the advisory lock, opens a fresh writer, performs
  all short database writes, and drops the handle.
- A lock collision retries with bounded exponential backoff: 25 ms, 50 ms,
  100 ms, 200 ms, 400 ms, 800 ms, then returns `BrainError::Locked` with the
  holder path. The total wait is under two seconds.
- A model, tokenizer, or embedder may live in a process-level `ModelRuntime`.
  It is independent of `Brain` and owns no store handle.
- Resolution state must be rebuilt or generation-validated on each write
  operation. No process may retain a mutable store-derived index after releasing
  the writer lock.

This preserves P8's purpose: no concurrent writers and no divergent mutable
in-memory indexes. Multiple applications can share a brain without a daemon
because no application monopolizes the lock.

### 3.2 Operation-scoped document cache handles

`documents.db` has its own advisory lock and WAL readers. It is a different store
with a different loss class, so its lock is independent of `brain.db`.

A document query performs:

1. scan configured roots outside any database transaction;
2. compute a pure reconciliation plan;
3. acquire the document-cache lock;
4. compare the plan's base generation with the current cache generation;
5. apply the plan in one short transaction, or rescan if another process won;
6. release the write handle;
7. run lexical/vector reads;
8. validate each materialized hit against the current file revision.

Two concurrent query processes can scan simultaneously. Only the short cache
apply is serialized. Losing a compare-and-swap race causes a rescan or no-op,
not corrupted state.

### 3.3 Session transports

`oxibrain-client` stops discovering a default socket. Its canonical local
transport spawns:

```text
oxibrain serve --stdio --dir <explicit-dir>
```

The child retains protocol state and optional loaded models but opens databases
per request. Oximemo, oxios, and oxiline may keep one child for their application
session. Tests must pass a temporary `--dir`; there is no default endpoint they
can accidentally reach.

External MCP hosts use the same stdio command. The MCP tool cap remains fifteen.
The embedded HTTP console uses the same request executor and is foreground-only.

The Rust `Brain` facade remains available for direct in-process callers. The CLI,
MCP, HTTP, and client transports all sequence the same facade operations.

## 4. Document root configuration

Roots come only from `~/.oxi/brain/documents.toml` or the equivalent file inside
an explicit `--dir`:

```toml
[[root]]
alias = "vault"
path = "~/.oxi/vault"
space = "personal"
include = ["**/*.md", "**/*.txt", "**/*.html"]
exclude = ["**/.git/**", "**/.DS_Store", "**/*.tmp", "**/*.lock"]
max_file_bytes = 10485760

[[root]]
alias = "handbook"
path = "~/work/handbook"
space = "work"
include = ["**/*.md"]
exclude = ["**/node_modules/**", "**/target/**"]
max_file_bytes = 10485760
```

Rules:

- `alias` is mandatory and globally unique.
- `path` is canonicalized at load time but never returned to an agent.
- `space` is mandatory; a missing space disables that root with a doctor error.
- `locator` is the normalized root-relative path with `/` separators.
- symlinks are not followed; path traversal outside the canonical root is
  rejected.
- unreadable, oversized, unsupported, and binary files are skipped and reported
  by `doctor`; they never fail the whole query.
- git roots additionally honor repository ignore rules through gix.
- plain roots use the explicit include/exclude lists. A `.gitignore` in a plain
  root has no special authority unless that root is also a discovered git
  worktree.
- a missing root is skipped and surfaced in freshness/doctor output; it does not
  make memory retrieval fail.

On a fresh or upgraded installation, oxibrain may seed exactly one known default:
`alias = "vault"`, `path = "~/.oxi/vault"`, `space = "personal"`, and only when
that directory exists. It never imports paths from the legacy `sources` table.
Custom roots require an explicit config edit.

## 5. gix integration

### 5.1 Existing ecosystem layer

`oxi-vault-git` already implements the write side correctly:

- direct blob/tree/commit creation with gix;
- no Git CLI process and no index dependency;
- content-deduplicated commits;
- shared ownership markers for oximemo and oxios;
- commit log, per-file log, diff, restore, tags, and integrity checks;
- lexical path-containment checks before file access.

Oxibrain does not replace or wrap this writer.

### 5.2 Oxibrain read side

`oxibrain-connectors` gains a read-only `GitDocumentReader` backed directly by
`gix`. It provides:

```rust
pub struct GitSnapshot {
    pub head: Option<String>,
    pub object_format: GitObjectFormat,
    pub tracked: BTreeMap<String, GitBlob>,
}

pub struct GitBlob {
    pub oid: String,
    pub bytes: u64,
}

pub struct DocumentRevision {
    pub root_alias: String,
    pub locator: String,
    pub revision: String,
    pub content: Vec<u8>,
    pub committed_at: Timestamp,
    pub source: RevisionSource,
}
```

It resolves HEAD through gix, so symbolic refs, detached HEAD, packed refs, and
linked worktrees are handled by the library rather than by reading `.git/HEAD`
manually.

The reader uses HEAD for history and cheap clean-file revision identity. Current
retrieval is still based on the worktree. A file is:

- `git:<format>:<blob-oid>` when its current bytes equal the HEAD blob;
- `blake3:<digest>` when dirty, untracked, or outside the HEAD tree.

This accommodates the index-less `oxi-vault-git` repository and the short window
between a file save and the authoring application's commit consumer.

### 5.3 History and rename semantics

Document references are path references:

```text
doc://<root-alias>/<locator>[@<revision>]
```

- unpinned references resolve the current locator;
- pinned references are allowed only when the revision is a git blob reachable
  from the repository;
- plain-root pinning is rejected because oxibrain stores no historical bytes;
- rename changes the locator and document ID, so an old reference dangles;
- gix history may offer a best-effort rename suggestion, but it never silently
  rewrites an immutable declaration;
- ambiguous or dangling references are reported by `doctor`.

Git does not store rename identity; it infers similarity between trees. The
architecture therefore makes no false rename-safety promise.

`episodes_for_locator` is removed rather than made to return non-episodes. Its
replacement is `document_history`, returning `DocumentRevision`. Legacy episode
chains remain in the ledger and are exposed only through an explicit
`legacy_document_history` operator path during migration review. Git commits are
never disguised as episodes.

## 6. Document cache database

`documents.db` starts at its own schema version 1. It is not `brain.db` schema
v11 and has no foreign keys into `brain.db`.

```sql
CREATE TABLE cache_meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE doc_roots (
  alias          TEXT PRIMARY KEY,
  space          TEXT NOT NULL,
  config_hash    TEXT NOT NULL,
  generation     INTEGER NOT NULL,
  head_revision  TEXT,
  scanned_at     INTEGER NOT NULL
);

CREATE TABLE documents (
  id           TEXT PRIMARY KEY, -- blake3(root_alias, locator)
  root_alias   TEXT NOT NULL REFERENCES doc_roots(alias) ON DELETE CASCADE,
  space        TEXT NOT NULL,
  locator      TEXT NOT NULL,
  revision     TEXT NOT NULL,
  bytes        INTEGER NOT NULL,
  modified_at  INTEGER NOT NULL,
  indexed_at   INTEGER NOT NULL,
  UNIQUE(root_alias, locator)
);

CREATE TABLE doc_manifest (
  root_alias   TEXT NOT NULL REFERENCES doc_roots(alias) ON DELETE CASCADE,
  locator      TEXT NOT NULL,
  bytes        INTEGER NOT NULL,
  modified_ns  INTEGER NOT NULL,
  revision     TEXT NOT NULL,
  PRIMARY KEY(root_alias, locator)
);

CREATE TABLE doc_chunks (
  id           TEXT PRIMARY KEY, -- blake3(document_id, revision, ordinal)
  document_id  TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  space        TEXT NOT NULL,
  ordinal      INTEGER NOT NULL,
  span_start   INTEGER NOT NULL,
  span_end     INTEGER NOT NULL,
  context      TEXT NOT NULL,
  UNIQUE(document_id, ordinal)
);

CREATE VIRTUAL TABLE doc_fts_word USING fts5(
  body,
  space UNINDEXED,
  chunk_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE VIRTUAL TABLE doc_fts_ngram USING fts5(
  body,
  space UNINDEXED,
  chunk_id UNINDEXED,
  tokenize = 'trigram'
);

CREATE VIRTUAL TABLE doc_vectors USING vec0(
  chunk_id TEXT PRIMARY KEY,
  embedding FLOAT[1024]
);
```

The FTS tables contain the lexical index of chunk text; `doc_chunks` does not
store a second plaintext copy. The source file remains authoritative.

`doc_vectors` cannot enforce a normal foreign key. Reconciliation must explicitly
delete vectors for every removed or changed document before deleting chunks.
Chunk IDs include revision, so a new revision can never inherit an old vector.

`cache_meta` stores:

- document schema version;
- chunker version;
- lexical tokenizer version;
- embedding model ID and digest;
- embedding dimension.

A chunker or tokenizer change rebuilds chunks and FTS. An embedding model or
digest change deletes all document vectors without touching lexical state.

`documents.db`, its WAL files, and all document rows are excluded from backup,
export, and truth reprojection. Deleting the database and rebuilding changes only
latency and, until optional embeddings are regenerated, dense-channel coverage.
Lexical membership must be identical after rebuild.

## 7. Reconciliation

### 7.1 Scan contract

Both git and plain roots perform a metadata walk on each document-target query.
A root-directory mtime is not a valid watermark because it misses in-place edits
and changes below nested directories.

The scan produces a sorted manifest:

```rust
pub struct FileObservation {
    pub locator: String,
    pub bytes: u64,
    pub modified_ns: i128,
    pub revision_hint: Option<String>,
}
```

The walk compares `(locator, bytes, modified_ns)` against `doc_manifest`.
Unchanged rows require no content read or digest. Added or changed rows are read
outside the cache transaction, with metadata checked before and after the read.
If the file changes during the read, that file is retried once and otherwise
reported as transiently skipped.

For git roots, gix supplies HEAD blobs and ignore rules. A clean observation may
use the blob OID without hashing bytes. Dirty and untracked observations use
BLAKE3. The Git index is neither required nor trusted as the source of the
current worktree.

### 7.2 Pure decision boundary

P9 applies explicitly:

```text
connector: scan filesystem + gix → RootObservation
store:     fetch cached manifest  → CachedRoot
core:      plan_reconcile(CachedRoot, RootObservation) → ReconcilePlan
facade:    read changed bytes, chunk, apply plan, sequence retries
store:     apply plan atomically
```

`plan_reconcile` is pure and returns every observed path in exactly one class:
`Unchanged`, `Add`, `Replace`, `Delete`, or `Skip(reason)`. It is property-tested
for conservation and deterministic ordering.

### 7.3 Exact lexical freshness

A successful document query guarantees:

- every stable supported file observed during its scan has current documents,
  chunks, and FTS rows before retrieval;
- deleted files have no document, chunk, FTS, or vector rows;
- a changed document is replaced atomically;
- no model or network call occurs during lexical reconciliation;
- missing and actively changing files are surfaced in `freshness.skipped`, not
  silently treated as current.

Initial indexing can take time proportional to corpus size. It is not hidden
behind an invented constant-time claim. The CLI reports files and bytes scanned.
`oxibrain index --documents` performs the same operation explicitly.

### 7.4 Dense freshness

Document embeddings are optional and never computed implicitly merely because a
lexical query ran.

- `oxibrain index --documents --embed` embeds every missing chunk.
- A session-scoped stdio or HTTP server may embed changed chunks only when its
  embedder is already loaded and the caller requested dense/hybrid mode.
- One-shot hybrid search may load the configured embedder to create the query
  vector, but it does not backfill the corpus unless `--embed` was requested.
- Missing vectors reduce dense coverage; lexical coverage remains exact.
- `SearchResponse` reports `dense_coverage = embedded_chunks / total_chunks`.

The same multilingual BGE-M3 model is used for entities and document chunks, but
the vector tables and rankings remain separate. Model identity, not dimension
alone, determines compatibility.

### 7.5 Hit materialization

FTS/vector retrieval first returns chunk IDs. Before returning text, the facade:

1. resolves root alias and locator through config;
2. rejects traversal and symlink changes;
3. reads the current file;
4. verifies its revision equals `documents.revision`;
5. verifies span boundaries against the bytes;
6. returns the verbatim slice.

If revision verification fails, the hit is discarded, that root is reconciled
once, and retrieval is retried once. A second change returns a freshness warning
rather than mismatched text. A hit is never labeled with a revision whose bytes
were not read.

## 8. Retrieval and context

`Query` gains:

```rust
pub enum SearchTarget {
    Memory,
    Documents,
}

pub struct Query {
    pub space: String,
    pub targets: BTreeSet<SearchTarget>, // default: both within this space
    // existing fields unchanged
}
```

The response is:

```rust
pub struct SearchResponse {
    pub memory: Vec<SearchResult>,
    pub documents: Vec<DocumentHit>,
    pub freshness: DocumentFreshness,
}

pub struct DocumentHit {
    pub document_id: String,
    pub root: String,       // config alias
    pub locator: String,
    pub revision: String,
    pub ordinal: u32,
    pub text: String,
    pub modified_at: Timestamp,
    pub score: f64,
}
```

Memory uses its existing channels. Documents use word FTS, trigram FTS, and the
optional vector channel, fused by RRF only within the document list.
`TargetId::Chunk` becomes the document candidate identity and reuses the pure
`rank` conservation contract.

Every query remains space-scoped. `targets = both` means both planes inside the
same authorized space; it never means all spaces.

`ContextInput` in core gains `documents: Vec<DocumentExcerpt>`. The facade fetches
memory from `brain.db`, documents from `documents.db` and the filesystem, then
calls pure `pack`. `Documents` is inserted between `QueryNeighborhood` and
`RecentEpisodes`. Each excerpt carries `[doc://alias/locator@revision]` and is
counted with `TokenizerPort` like every other layer.

Legacy `document` and `document_revision` episodes remain in the ledger but are
excluded from default memory search, recent-episode context, and extraction.
This prevents duplicate document hits across the two lists while preserving the
append-only ledger for explicit operator review or redaction.

## 9. Memory extraction without a queue

### 9.1 Eligibility

All new `Primary` episodes are memory-plane inputs. Documents never become new
episodes. Legacy primary episodes with `source_kind IN ('document',
'document_revision')` are permanently ineligible for extraction.

The backlog query becomes `uncached_memory_episodes`:

```sql
SELECT e.id
FROM episodes e
WHERE e.space_id = ?1
  AND e.kind = 'primary'
  AND e.source_kind NOT IN ('document', 'document_revision')
  AND e.redacted_at IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM extractions x
    WHERE x.episode_id = e.id AND x.extractor_id = ?2
  )
ORDER BY e.seq;
```

The absence of a matching extraction is the entire pending state. There are no
leases, attempts, or job-state transitions.

### 9.2 Capture semantics

`remember`, explicit agent capture, and `ingest` follow the same sequence:

1. short transaction appends the primary episode;
2. transaction closes;
3. LLM extraction runs;
4. validation runs;
5. short writer operation stores extraction/assertions and folds beliefs;
6. command returns episode ID, extraction outcome, and pending count.

The episode write is the success boundary. If model loading, inference,
validation, or the second write fails, the command returns `captured_pending`,
not a false rollback. `oxibrain extract --pending [--limit N]` rediscovers it.
`stats` and `doctor` always show eligible pending count and oldest pending age.

There is no automatic background drain. Explicit capture tries inline;
`extract --pending` is the operator-controlled repair and batch path. A
session-scoped server may retain the model between explicit requests but does not
start a timer or worker.

`extraction_failures` remains durable and auditable. It records semantic failures
and is independent of the removed queue.

## 10. Components removed

The clean cutover deletes:

- daemon subcommand and daemon process;
- launchd integration and well-known socket discovery;
- `BrainServer::start_source_watchers`;
- debounced oxibrain vault watcher;
- `sync/run` RPC and `oxibrain sync`;
- pull-source registration and `ensure_source_impl` for document roots;
- vault scan/classify/occurrence ingestion into episodes;
- daemon extraction worker;
- `ingest_jobs`, enqueue/claim/lease/complete/fail/reclaim APIs;
- `Brain::extract_pending` lease loop and `Brain::job_status`;
- client code that silently attaches to the production socket.

They are replaced by:

- explicit `documents.toml`;
- `documents.db` and request-time reconciliation;
- read-only gix integration;
- `oxibrain index --documents [--embed]`;
- `uncached_memory_episodes` and `oxibrain extract --pending`;
- operation-scoped database handles;
- explicit stdio/HTTP session processes.

`serve` remains a foreground/session transport command, never a daemon.

## 11. Migration

### 11.1 `brain.db` schema v11

The v11 migration:

1. drops `ingest_jobs`;
2. leaves every episode and source row untouched;
3. removes `ingest_jobs` from `EXPORT_TABLES`;
4. makes import skip removed operational tables with a logged compatibility
   notice rather than failing a pre-v11 export;
5. bumps the projection version so default memory indexes rebuild without legacy
   document episodes.

Pull source rows cannot be deleted because legacy episodes and source policies
reference them. They remain provenance-only legacy rows. No runtime code lists,
watches, or writes them after migration.

Before dropping the queue, migration records the eligible pending count for the
post-migration report. The query-derived backlog recovers every eligible episode;
legacy document jobs intentionally disappear.

### 11.2 Document cache initialization

`documents.db` is new and empty. It is built from configured roots on first
`search`, `recall`, or `index --documents`.

The migration never copies paths from `sources`, so the 1018 temporary-directory
rows cannot infect the new config. `doctor` may display existing, currently valid
legacy pull paths as suggestions, but it never edits `documents.toml` from them.

### 11.3 Legacy history

Legacy document episodes remain queryable only through the explicit operator
history surface until redacted. New document history comes from gix and returns
`DocumentRevision`, not `Episode`.

No migration fabricates git commits, rewrites a vault, or copies document bytes
into the new database.

### 11.4 Ecosystem cutover

This is a clean cutover across first-party consumers:

- oximemo/oxios stop registering vault sources and stop calling `sync/run`;
- their existing `oxi-vault-git` usage remains unchanged;
- they spawn an explicit stdio oxibrain session for memory features;
- oximemo `HistoryPanel` consumes `document_history` directly from gix-backed
  oxibrain output or its already-linked `oxi-vault-git` layer;
- oxiline memory writes use explicit stdio requests;
- tests use temporary directories and cannot discover production state;
- the official OMP integration launches the stdio MCP command or invokes the CLI
  with an explicit `--dir`; it never requires a daemon.

## 12. Failure handling and security

- **Missing root:** skip, return freshness warning, report in `doctor`.
- **Corrupt git metadata:** fall back to plain worktree indexing for current
  retrieval; disable git history; report in `doctor`.
- **Foreign git repository:** read-only indexing/history is allowed; oxibrain
  never adopts or writes it.
- **Concurrent file modification:** retry one file once; otherwise skip it for
  this query and report it.
- **Concurrent cache writers:** generation compare-and-swap; loser rescans.
- **Concurrent memory writers:** bounded advisory-lock retry; no hidden remote
  fallback.
- **Model unavailable:** lexical document retrieval still works; memory capture
  returns `captured_pending`; dense coverage is reported.
- **Malformed config:** invalid roots are disabled individually; duplicate aliases
  are a hard config error because document references would be ambiguous.
- **Path escape:** absolute locators, `..`, symlinks, and post-open canonical path
  changes are rejected.
- **Scope:** config root space must equal the authorized query space; no root or
  hit from another space enters candidate ranking.
- **Privacy:** agent-visible results contain alias and locator, never the absolute
  root. Ignore/exclude rules apply before bytes enter FTS.

## 13. Invariants and verification

Each invariant requires a behavioral or property test that fails on a plausible
bug.

1. **No document reaches truth.** Index a document containing an obvious triple;
   truth tables remain byte-identical and no extraction is invoked.
2. **Physical separation.** Drop `documents.db`; every `brain.db` ledger/truth
   snapshot is unchanged.
3. **Lexical rebuild equivalence.** Delete and rebuild `documents.db`; supported
   document membership and lexical recall@10 are identical.
4. **Plain in-place edit detection.** Edit a nested file without changing the
   root directory mtime; the next query returns the new text only.
5. **Index-less gix compatibility.** Create commits by direct tree editing with
   an empty Git index; gix discovery returns the HEAD files and history.
6. **Dirty worktree freshness.** Change and add files without committing; the
   next query uses BLAKE3 revisions and returns current bytes.
7. **Deletion completeness.** Delete a file; document, chunks, both FTS tables,
   and vectors contain no reachable row.
8. **Vector revision safety.** Replace a document at the same locator and
   ordinals; no old vector survives, and new chunk IDs differ.
9. **Model invalidation.** Change embedding model digest at the same dimension;
   vectors are cleared and dense coverage reports zero until rebuilt.
10. **Materialization race.** Modify a file between FTS hit and slice; no
    mismatched text/revision pair is returned.
11. **Reconcile conservation.** Every observed/cached locator lands in exactly
    one reconciliation class; order is deterministic.
12. **Concurrent cache writers.** Two processes reconcile the same changed root;
    final rows match one serial reconciliation with no duplicate or orphan.
13. **Daemonless readers.** Multiple processes read memory concurrently while a
    short memory writer completes.
14. **Daemonless writers.** Two one-shot memory writers contend; both complete in
    serial order or one receives the bounded, explicit lock error—never partial
    state.
15. **No production ambient authority.** A process with a temp `--dir` has no API
    or discovery path that can open `~/.oxi/brain`.
16. **Pending recovery.** Crash after episode commit but before extraction write;
    `extract --pending` finds exactly that eligible episode.
17. **Legacy exclusion.** Legacy document primary episodes never enter extraction,
    memory search, or recent context.
18. **Migration FK safety.** Upgrade the production-shaped v10 fixture with
    episodes referencing pull sources; migration succeeds and references remain.
19. **Cross-version import.** Import a pre-v11 export containing `ingest_jobs`;
    durable tables import and the removed operational rows are logged/skipped.
20. **Space isolation.** A both-target query returns memory and documents from the
    authorized space only.
21. **P11 parity.** Document lexical recall gap across writing-system property
    classes stays within ten percentage points.
22. **MCP cap.** The stdio server still exposes at most fifteen MCP tools.
23. **Session lifetime.** Closing stdio terminates the child and leaves no socket,
    launchd job, database lock, or background worker.

## 14. Documentation and contract changes

Acceptance of this spec requires one coordinated documentation revision, not
stale follow-up notes:

- `ARCHITECTURE.md`
  - P8 becomes operation-scoped one-writer-per-store;
  - deployment modes remove daemon ownership;
  - data flow separates document cache from episodes;
  - zones list `documents.db` as disposable ranking cache;
  - extraction stages remove the durable job state machine;
  - product shape replaces daemon subcommand with session transports;
  - Foundation socket discovery is removed.
- ADR-010 becomes Superseded.
- ADR-011 keeps consumer-owned git writes and replaces semantic occurrence
  history with read-only gix document history.
- ADR-007 socket/discovery clauses are superseded by explicit stdio process
  launch; auth/scope semantics remain for protocol sessions.
- `CONSUMPTION_CONTRACT.md` removes `sync/run`, socket discovery, and
  `episodes_for_locator`; adds explicit stdio launch and `document_history`.
- `ECOSYSTEM.md` replaces watcher/daemon integration with explicit documents
  config and session child processes.
- CLI help and operator docs state that no service installation is required.

## 15. Operator decision for this machine

After implementation, use a fresh `documents.db`; there is nothing to migrate
from the old document projection.

For `brain.db`, preserve the ledger through v11 first. Then export and review the
three current beliefs. Redaction or a fresh store remains an explicit operator
decision; migration does not destroy ledger history.

The canonical document config for this machine is:

```toml
[[root]]
alias = "vault"
path = "~/.oxi/vault"
space = "personal"
include = ["**/*.md", "**/*.txt", "**/*.html"]
exclude = ["**/.git/**", "**/.DS_Store", "**/*.tmp", "**/*.lock"]
max_file_bytes = 10485760
```

The existing gix HEAD tree is valid despite the empty index. No git repair or
history restart is required.

## 16. Final architecture statement

The completed system has no ambient service:

```text
configured files ──scan/gix──> documents.db ──document hits──┐
                                                              ├─> search/recall
explicit capture ────────────> brain.db ──────beliefs────────┘

caller ──CLI / stdio child / foreground HTTP──> operation-scoped handles
```

The filesystem and git own documents. The ledger owns memory. SQLite caches make
both searchable. Models may stay warm inside a caller-owned session, but no
process owns the brain when nobody is using it.
