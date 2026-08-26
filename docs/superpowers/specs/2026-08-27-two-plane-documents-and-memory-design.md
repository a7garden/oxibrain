# Two Planes: Documents are a Cache, Memory is a Ledger — Design

> **Date:** 2026-08-27 · **Status:** draft, awaiting user review
> **Scope:** Split what the brain *keeps* from what the brain *believes*. Vault
> documents become a rebuildable retrieval cache and leave the ledger; the truth
> half is fed only by explicit capture and declaration. Deletes the pull-source
> registry, the extraction queue, the vault watcher, and the daemon's background
> worker.
> **Supersedes on acceptance:** ADR-010 (daemon-hosted vault watch). Amends
> ADR-011 (§4.5). Requires `ARCHITECTURE.md` §4.2/§4.3/§5.1/§16 revision.

## 1. Problem

### 1.1 Production evidence (measured 2026-08-27, `~/.oxi/brain/brain.db`)

| Observation | Value |
|---|---|
| `sources` rows | 1024 — of which **1018 are macOS temp directories** |
| `episodes` / `ingest_jobs` / `beliefs` / `entities` | 301 / **216** / 3 / 5 |
| Vault git (`~/.oxi/vault`) | 37 commits, `.oxi-vault-git` marker, **0 tracked files** |

`daemon.log` shows the consequence of row 1: on every start the daemon
enumerates all registered pull sources and tries to adopt each into a watcher —
hundreds of `vault watcher sync failed: not a directory:
/private/var/folders/…` lines, including `oximemo-cli-test-88234-*`. An
oximemo test suite connected to the well-known default socket and registered
throwaway vaults in the user's real brain.

Row 2 is the second failure: the daemon's background extraction worker
(`oxibrain-mcp/src/daemon.rs:83-105`, periodic `extract_pending`) has left 216
jobs queued and produced 3 beliefs from 301 episodes. **Nobody noticed**, which
is itself the finding: nothing in daily use depended on beliefs extracted from
vault documents.

### 1.2 Structural diagnosis

P1 (immutable ledger + rebuildable projection) was applied to the knowledge half
and not to the operational half. Three pieces of non-derived mutable state drive
runtime behaviour, and all three rotted:

| State | Duplicates | Failure |
|---|---|---|
| `sources` table | "which roots matter" — user intent | 1018 ghosts; drives watcher adoption |
| `ingest_jobs` table | "what lacks extraction" — a set difference over `episodes`/`extractions` | 216 stuck, silent |
| watcher set | "what changed" — the filesystem and git already know | watches dead paths; debounce loses intermediate states |

Each **stores the answer to a question that can be asked**. The codebase already
proves the queue is redundant: `uncached_episodes`
(`oxibrain-store/src/extraction.rs:510`) computes the extraction backlog as a
query for the `reextract` path, alongside the durable queue that broke.

### 1.3 The content argument (decisive)

The vault holds fiction the user writes, article clippings, song lyrics, study
material, idea drafts, scratch memos, work convention documents, and company
standard docs. Extraction over this corpus is not merely expensive, it is wrong:

- **Fiction** yields entities for characters and assertions about them. P3
  re-resolution can merge a character into a real person of the same name.
- **Clippings and lyrics** are third-party claims and metaphor. Folding them
  makes a brain that asserts whatever it read.
- **Study material** turns other people's knowledge into the user's beliefs.
- **Drafts** contradict current thinking and generate resolution work nobody
  asked for.
- **Convention docs** are the worst case: an agent needs the *verbatim rule with
  its source*, and a temporal fold picks one winner instead of showing an old
  and a new convention side by side.

Across every category, extraction is useless or harmful. The principle:
**the brain believes only what it was told, and retrieves everything it was
given.**

### 1.4 Code findings that shape the design

Verified by reading the tree (2026-08-27):

1. **Lexical indexing is already free at ingest.** `ingest_and_enqueue`
   (`oxibrain-store/src/extraction.rs:555-587`) inserts the episode and calls
   `index_episode_fts` (`index_ops.rs:76-85`) inside the same write
   transaction — `fts_word` (unicode61) and `fts_ngram` (trigram), no LLM.
2. **Everything else in the ranking half is rebuild-only.** `chunks`,
   `tfidf_vectors`, and `entity_vectors` are produced by `rebuild_indexes`
   (`index_ops.rs:535-541`) under `reproject`. There is no incremental path.
3. **Embeddings are entity-scoped.** `entity_vectors` is a vec0 table keyed by
   `entity_id`, filled in `Brain::reproject` (`oxibrain/src/lib.rs:281-283`).
   Dense retrieval therefore *rides on the truth half* — precisely what we are
   removing for documents. There is no document or chunk vector today.
4. **`chunks` is dead scaffolding.** The table exists (`migrations/v8.sql`), and
   `TargetId::Chunk` / `VecSpace::Chunk` exist, but no retrieval channel emits a
   chunk id.
5. **`search` cannot return a document.** `search_results`
   (`store/query.rs:352-388`) projects `TargetId::Entity` only, and `snippet` is
   `format!("matched: {}", predicate)` — not text.
6. **`recall` already returns verbatim text.** `EpisodeExcerpt.content` reaches
   the answer through `pack::render_episodes` (`core/pack.rs:493-538`), top-k
   rendered in full.
7. **Retrieval does not require beliefs.** Lexical channels hit episodes
   directly; `Episode` targets receive `minimal_facts` padding
   (`core/rank.rs:530-545`) and survive ranking at `min_confidence = 0`.
8. **Vault documents and captures are indistinguishable by `kind`.** Both are
   `EpisodeKind::Primary` (`core/types.rs:76-105`); only `source_kind` /
   `source_id` / `source_ref` differ.
9. **Trust is not actually server-evaluated.** `effective_policy_trust`
   (`ledger.rs:640`) has no production caller; MCP `resolve_trust` reads the
   payload argument. Documented invariant, unimplemented.
10. **`sources` has a non-declaration write path.** Besides
    `Declaration::RegisterSource` (`project.rs:730`), `ensure_source_impl`
    (`oxibrain/src/ingest.rs:154`) inserts directly — this is how test temp dirs
    became permanent registry rows without any ledger record.

## 2. Decision

**Two planes, different persistence classes, one query path.**

| | Memory plane | Document plane |
|---|---|---|
| Holds | what the brain was **told** | what the user **keeps** |
| Sources | `remember`, agent capture, `declare`, curation, oxiline events | vault files and other document roots |
| Storage | `episodes` ledger → extraction → assertions → fold → beliefs | `documents` + `doc_chunks` + `doc_vectors` |
| Class | **truth**, immutable, P1 reprojection | **cache**, droppable, rebuildable from the filesystem |
| Volume | tens per day | thousands of files |
| LLM | synchronous at capture | never |
| Deletion | `redact()`, audited (P5) | delete the file; next reconcile removes it |
| Answer shape | folded belief + uncertainty + sources (P10) | verbatim chunk + path + revision |

The document plane is the **ranking half only**. `ARCHITECTURE.md` §5.1 already
says nothing in the ranking half may be read by the fold; this design makes
document content structurally unable to reach it, because it never becomes an
episode.

## 3. Non-goals

- **Deleting existing document episodes.** The ledger is append-only; legacy
  `document_revision` episodes stay and remain lexically searchable. Purging is
  an operator choice via the sanctioned `redact()` path (§8), never a migration.
- **A sixteenth MCP tool.** The cap is fifteen (`ARCHITECTURE.md` §16.2).
  Document retrieval extends `search` with a parameter (§4.4).
- **Owning vault history.** ADR-011 stands: git is the mechanical history layer
  and works with the brain absent. This design *reads* git; it never writes to a
  vault (C3).
- **Removing the daemon.** It becomes optional (§4.8), not deleted — long-lived
  GUI clients still want a serving process.
- **Cross-plane score fusion.** Beliefs and document chunks are not commensurable;
  results are two lists, not one blended ranking (§4.4).

## 4. Design

### 4.1 Document identity

A document is `(root_id, locator)` where `root_id = blake3(canonical root path)`
and `locator` is the root-relative path with forward slashes. A *revision* is
identified by its content digest:

- **Git-backed root:** the blob SHA from the commit tree. Free, exact, already
  content-addressed, and rename-tracked by git.
- **Plain root:** `blake3(bytes)`.

No occurrence chain, no predecessor derivation, no debounce window. The brain
stores the current revision only; history belongs to git (ADR-011).

### 4.2 Storage (schema v11, all three tables are cache)

```sql
CREATE TABLE documents (
  id         TEXT PRIMARY KEY,        -- blake3(root_id, locator)
  space_id   TEXT NOT NULL REFERENCES spaces(id),
  root_id    TEXT NOT NULL,
  locator    TEXT NOT NULL,
  revision   TEXT NOT NULL,           -- git blob sha, or blake3(bytes)
  bytes      INTEGER NOT NULL,
  modified_at INTEGER NOT NULL,       -- commit time, else mtime
  indexed_at INTEGER NOT NULL,
  UNIQUE (root_id, locator)
);

CREATE TABLE doc_chunks (
  id          TEXT PRIMARY KEY,       -- blake3(document_id, ordinal)
  document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
  space_id    TEXT NOT NULL,
  ordinal     INTEGER NOT NULL,
  span_start  INTEGER NOT NULL,
  span_end    INTEGER NOT NULL,
  context     TEXT NOT NULL,          -- deterministic prefix (§9.3)
  embedded    INTEGER NOT NULL DEFAULT 0,
  UNIQUE (document_id, ordinal)
);

CREATE VIRTUAL TABLE doc_vectors USING vec0(
  chunk_id TEXT PRIMARY KEY, embedding FLOAT[1024]
);
```

Chunk text is not stored; `span_start`/`span_end` address the file, matching the
existing `chunks` convention (`migrations/v8.sql`). Reads slice the file at
answer time, so a stale index can never emit text the file no longer contains.

Chunking reuses `oxibrain_core::chunking::{split_into_chunks,
render_context_prefix}` — already pure, already deterministic.

`documents`/`doc_chunks`/`doc_vectors` are excluded from `EXPORT_TABLES`
(`store/export.rs:24-28`) and from the truth-half determinism test. Dropping all
three and reindexing must be a no-op observable only in latency.

### 4.3 Reconciliation: cheap check, lazy trigger, two freshness classes

**Roots come from user-owned config**, not a mutable table:

```toml
# ~/.oxi/brain/documents.toml
[[root]]
path  = "~/.oxi/vault"
space = "personal"

[[root]]
path  = "~/work/handbook"
space = "work"
```

This kills the 1018-ghost failure class structurally: no client can append a
root, and a test that syncs a temp directory writes nothing durable. `sources`
survives only for **push** provenance on the memory plane; `mode = 'pull'` and
`kind = 'document_revision'` are retired, as is the `ensure_source_impl`
non-declaration write path (finding 1.4-10).

**Watermark check** (`ensure_fresh(space)`, called at the top of every query
path):

| Root kind | Check | Cost |
|---|---|---|
| git | read `.git/HEAD` + the ref file; compare to stored watermark | two small file reads |
| plain | root `mtime` compared to stored watermark | one `stat` |

Unchanged → return immediately. This is the whole reason no resident process is
needed: the check is cheap enough to run on every query, so freshness becomes a
property of asking rather than of something running.

**Delta computation** when the watermark moved:

- git root: `git diff --name-status <watermark>..HEAD` — exact adds, modifies,
  deletes, renames, in O(delta). If the stored watermark is unreachable (history
  rewritten, shallow clone), fall back to a full tree walk.
- plain root: directory walk comparing `(size, mtime)` then digest.

Implemented in `oxibrain-connectors` (an adapter crate, may depend on `gix`);
`oxibrain-core` stays free of I/O, and the standalone guarantee is unaffected
because `gix` is not an oxi crate.

**Two freshness classes** — the design's load-bearing compromise:

1. **Exact, always:** `documents`, `doc_chunks`, and chunk FTS. No model, no
   network; a delta of hundreds of files is inserts and stays inside a query's
   latency budget.
2. **Eventually complete:** `doc_vectors`. Embedding is the only expensive step,
   so it is backfilled under a per-invocation budget (default 32 chunks) with
   `doc_chunks.embedded = 0` marking the remainder. Lexical retrieval covers the
   gap in the meantime, and `oxibrain index --embed` drains it on demand.

An answer is never blocked on the GPU, and the plane that costs nothing is never
stale.

### 4.4 Retrieval: one path, two result kinds

`search` gains `targets: ["memory" | "documents"]` (default both) — a parameter,
not a sixteenth tool. `Query` gains the same field.

- **Memory targets** keep today's channels and today's `SearchResult`.
- **Document targets** add `Channel::Document{Lexical}` over chunk FTS and
  `Channel::Document{Vector}` over `doc_vectors`, fused by the existing
  `Fusion::Rrf { k: 60 }` **within the document plane**, producing:

```rust
pub struct DocumentHit {
    pub document_id: String,
    pub root: String,        // config alias, not an absolute path
    pub locator: String,
    pub revision: String,
    pub ordinal: u32,
    pub text: String,        // verbatim slice, read at answer time
    pub modified_at: Timestamp,
    pub score: f64,
}
```

The two lists are returned side by side. Blending a folded belief and a document
chunk into one ranking would invent a comparison that has no meaning; keeping
them separate is also what lets an agent see two conflicting convention
documents with their dates instead of one fold-selected winner (§1.3).

`recall` (context assembly) gains a `Documents` layer between
`QueryNeighborhood` and `RecentEpisodes`, carrying `provenance = [locator@revision]`
per the existing `ContextLayer` contract (`store/context.rs:11-18`). This is the
path that feeds convention docs to an agent verbatim.

`rank` is unchanged: `TargetId::Chunk` finally gets a producer, and
`minimal_facts` (`core/rank.rs:530-545`) already handles targets with no beliefs.
Conservation (`items ∪ dropped`) applies to document candidates for free.

### 4.5 The bridge: memory may point at a document

A capture or declaration may reference a document:

```
doc://<root-alias>/<locator>[@<revision>]
```

Unpinned references track the current revision; pinned ones name a git blob.
"This handbook section is our team standard" is then a `Declaration` in the
ledger — a first-class assertion with provenance — whose object is a document
reference, while the document's *content* is never an assertion. P2 holds
exactly: the brain records that the user declared something about a document, not
the document's sentences as claims.

Rename safety comes from git following the path; a plain-root rename appears as
delete + add and an unpinned reference dangles, surfaced by `oxibrain doctor`.

**ADR-011 amendment.** `episodes_for_locator` / `episodes/for_locator` /
`BrainClient::episodes_for_locator` (Consumption Contract 1.3) lose their data
source for new documents. They are re-specified as a git-backed read in
`oxibrain-connectors`, returning the same shape from the commit history.
oximemo's `HistoryPanel` then satisfies C1 better than today: it works with the
daemon stopped. Legacy occurrence chains already in the ledger remain queryable
through the same call.

### 4.6 Memory plane: synchronous extraction, no queue

With documents gone, everything extractable is explicitly authored and
low-volume. Extraction therefore runs **inline at capture**, inside `remember` /
agent capture, and the LLM call stays outside the write transaction as today
(§7.2).

`ingest_jobs` is dropped. The backlog becomes a query — the mechanism the
codebase already has in `uncached_episodes` (`store/extraction.rs:510`):

```
oxibrain extract --pending [--limit N]
```

selects primary episodes with no extraction row and processes them under a
budget. A crashed or failed extraction leaves the episode in the ledger and the
query rediscovers it; there is no lease to expire, no state machine to wedge, and
no silent 216-deep hole. `extraction_failures` keeps its no-silent-drop role.

Deleted with the queue: `enqueue_job`, `claim_jobs`, `complete_job`, `fail_job`,
`reclaim_expired` (`store/extraction.rs:71-201`), `Brain::extract_pending`'s
lease loop (`oxibrain/src/extraction.rs:242-299`), and `Brain::job_status`
(`lib.rs:1153`).

### 4.7 What is deleted

| Component | Location |
|---|---|
| Daemon extraction worker | `oxibrain-mcp/src/daemon.rs:83-105` |
| Vault watcher adoption | `BrainServer::start_source_watchers`, `oxibrain-mcp/src/server.rs` |
| Debounced watcher | `oxibrain-connectors::watch::spawn_quiet` |
| Scan/classify/occurrence sync | `oxibrain/src/vault.rs` (`sync_vault`, `ingest_event_one`, `pull_sources`) |
| `sync/run` RPC and `oxibrain sync` | `oxibrain-mcp/src/server.rs`, `oxibrain-cli/src/cmd/sync.rs` |
| Extraction queue | `store/extraction.rs:71-201` + callers |
| Pull-source registry path | `ensure_source_impl` (`oxibrain/src/ingest.rs:154`), `mode='pull'` rows |

Replaced by: `documents.toml`, `ensure_fresh`, a git/plain delta reader in
`oxibrain-connectors`, `oxibrain index [--embed]`, and `oxibrain extract --pending`.

### 4.8 The daemon becomes optional

Nothing left requires a resident process: no watcher to host, no queue to drain.
The daemon remains a *serving* topology for long-lived GUI clients (oximemo,
brain UI). CLI and terminal agents use embedded mode per invocation, which also
removes the ambient-authority hazard that let a test suite write to the
production brain — a one-shot CLI with an explicit `--dir` has no well-known
socket to reach.

**Concurrency is part of this design, not a follow-up.** A daemon that is
optional in theory but mandatory in practice is not optional. Today every CLI
command — including pure reads — goes through `Brain::open`, which takes the
exclusive advisory flock (`store/lock.rs:16-31`, fail-fast `BrainError::Locked`).
So a running daemon blocks even `oxibrain ask`, and stopping the daemon breaks
any GUI client. Both halves of that trap must go:

1. **Read commands route through `Brain::open_ro`.** The method already exists
   and is already documented as "No advisory lock, no writer actor. Can coexist
   with a running daemon — WAL mode allows concurrent readers"
   (`oxibrain/src/lib.rs:106-108`) — and no CLI command uses it. Converting
   `ask`, `page`, `entity show`, `contradictions`, `stats`, `why`, `timeline`,
   `export`, and `doctor` is a routing change, not new machinery.
2. **Write commands take the lock for the operation, not the process.** Open
   exclusive, do the work, drop the handle, with a bounded retry on `Locked`
   (default 5 attempts over ~2 s) so two concurrent one-shot writers queue
   instead of failing.

With both in place the topologies compose: any number of concurrent readers,
one writer at a time, daemon present or absent. The daemon's only remaining
justification is **model residency** — it keeps the embedding and extraction
models loaded, which matters for dense search and capture-time extraction and
not at all for lexical or document retrieval, neither of which touches a model.
It is therefore an opt-in performance mode, and daemonless is the default.

## 5. Invariants and tests

New invariants, each with a test that fails on a plausible bug:

1. **No document content reaches the fold.** No code path may create an
   assertion whose provenance is a `documents` row. Enforced structurally
   (documents are not episodes) and asserted by a test that indexes a document
   containing an obvious triple and asserts `beliefs` stays empty.
2. **The document plane is a pure cache.** Drop `documents`, `doc_chunks`,
   `doc_vectors`, run `oxibrain index`, and every truth-half table is
   byte-identical while document retrieval returns the same hits. Extends the
   existing reprojection determinism test rather than replacing it.
3. **Deletion is real.** Remove a file, reconcile, assert its chunks are gone and
   it is unreachable from `search`.
4. **Watermark no-op.** Reconcile twice with no filesystem change; the second
   pass performs zero writes and no digest computation.
5. **Delta correctness on git.** A → B → A and a rename each produce the correct
   final index state; a rewritten history falls back to a full walk.
6. **Freshness classes.** With embedding disabled, a fresh document is lexically
   retrievable in the same call; `embedded = 0` marks the backlog; `--embed`
   drains it.
7. **Conservation over document candidates.** Every document candidate lands in
   exactly one of `items` / `dropped` (existing `rank` property, new target type).
8. **Parity (P11).** The document retrieval metric gap across writing-system
   classes stays within 10 points — the existing parity suite gains document
   cases, since chunking and trigram FTS are the language-sensitive parts.
9. **Migration.** v10 → v11 up-test from a fixture; legacy `document_revision`
   episodes still readable and still lexically retrievable afterwards.
10. **Daemonless concurrency.** With a daemon holding the store, every read
    command still succeeds (`open_ro`, no lock). With no daemon, two concurrent
    one-shot writers both complete — one retries, neither fails. This is the
    test that keeps "the daemon is optional" true.

## 6. Migration (schema v11)

Additive: create the three cache tables, drop `ingest_jobs`. No episode is
touched, so P1 holds and the truth half is byte-identical across the migration.
`documents.toml` is created on first run from any surviving
`mode='pull'` source rows, then those rows are deleted — a one-time, logged,
user-visible translation from mutable registry to owned config.

## 7. Operator note for this machine

The production store currently holds 3 beliefs and 5 entities against 1024
source rows and 216 stuck jobs. The migration path above preserves it, but the
honest observation is that almost nothing there is worth preserving. Two options
after the code lands, user's choice:

- **Keep:** run the migration; legacy document episodes stay as vestigial but
  searchable history.
- **Restart:** `oxibrain export` the handful of real beliefs, init a fresh store,
  re-import. Costs minutes and leaves no ghosts.

Either way, the oximemo test suite must stop defaulting to the production socket
— otherwise the class of failure returns through a different door.

## 8. Open questions

1. **Root aliases in `DocumentHit`.** Returning `root = "vault"` instead of an
   absolute path avoids leaking home directory structure to an agent. Confirm
   that is wanted, and whether `locator` should also be redactable per root.
2. **Work/personal isolation.** Config maps a root to a space, so scoping already
   works. Should `search` default to the caller's space only, rather than both?
3. **Chunk embedding model.** `doc_vectors` reuses the 1024-dim BGE-M3 space of
   `entity_vectors`. Sharing dimensionality is convenient but the two spaces are
   never compared; confirm we want one model for both.
4. **`.gitignore` semantics.** Should the indexer honour the vault's
   `.gitignore` for plain roots too? For git roots it comes free.
