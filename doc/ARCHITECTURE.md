# oxibrain — Architecture
> **Version:** v2.12 · **Date:** 2026-08-28 · Supersedes `DESIGN.md` v1.0 (and v0.3–v0.1)
> **v2.12 — Space lifecycle (spec 2026-08-27).** Spaces are a managed unit:
> `oxibrain space add|remove|default`, `~/.oxi/config.toml` (`default_space`,
> strict parse, first §18 key implemented), per-space vault provisioning at
> `~/.oxi/vault/<space>/` with legacy flat-root `exclude` fixup, and P5 purge
> via `RedactTarget::Space` (audited; documents cache swept; vault files never
> deleted). Implicit space creation is abolished outside `init`, `space add`,
> and archive imports; the default space resolves as `--space` > config.toml >
> `"personal"` on CLI and MCP alike. Workspace `0.8.0 → 0.9.0`; Consumption
> Contract 1.5.
> **v2.11 — Two planes, no daemon (spec 2026-08-27).** Documents move to a
> separate `documents.db` cache rebuilt from configured files and gix history.
> `Brain` becomes a handle-free runtime facade: read methods open their store
> per call, write methods acquire the lock with bounded exponential backoff
> ([25, 50, 100, 200, 400, 800] ms then `Locked`) and drop the handle. The
> daemon, watcher, socket discovery, sync, and durable job queue are deleted;
> callers use the CLI, a caller-owned `serve --stdio` child, or foreground
> `serve --http`. `search` and `recall` accept `planes` (`Memory` /
> `Documents` / both, default both) and return `SearchResponse { memory,
> documents, freshness }`. Extraction is queue-less: remember/capture append
> inline, `extract --pending` loops `uncached_memory_episodes`, legacy
> document/document_revision episodes are excluded from memory search/context
> but preserved in the ledger. Memory schema v11 drops `ingest_jobs`.
> Coordination: ADR-010 is superseded; ADR-011 amended (§4.5 gix read-only);
> ADR-007 socket/discovery clauses superseded by explicit stdio process
> launch; Consumption Contract 1.4 records the breaking changes. Workspace
> `0.6.0 → 0.7.0`; `oxibrain-client 0.7.0 → 0.8.0`.
> v2.10 — Space enumeration and first-party RPC. Spaces are enumerable
> (`Brain::list_spaces`, `oxibrain spaces`, `spaces://` resource, native
> `spaces/list` RPC) — scoped sessions see only their membership, and
> `resources/read` is now scope-gated like tools (the gap was found and fixed
> with tests). §16.1 is aligned with ADR-009: `Brain` is the embedded surface,
> `oxibrain-client` the remote surface; the one-trait unification is post-v1
> with a stated trigger. The fifteen-tool MCP cap is unchanged — first-party
> operations ride the native RPC layer.
> v2.9 — Embedded repair/operations console. The console ships inside the `oxibrain`
> binary via `include_dir!` against the bundle committed inside `oxibrain-mcp`
> (`crates/oxibrain-mcp/assets/dist/`, vite's outDir — §16.6). `oxibrain serve --http
> 127.0.0.1:18080` now serves the UI without `--ui-dir`. The console's scope is
> repair/operations only (ecosystem blueprint §6.4): Overview, Entity, Conflicts, Merges,
> Failures, Sources, Operations. It is **not** a host for ask/chat, capture/authoring, an
> exploratory force graph, or general note/task/session management — those remain out of
> scope for oxibrain. ADR-008 locks the decision; CI gates `dist/` (built bundle matches
> committed tree, gzipped ≤ 400 KB).
> v2.8 — Curation parity. §4.2.2 documents the curation surface: every user
> knowledge edit (entity merge/split/alias/retract, manual declare, predicate add,
> source policy) is a `Declaration` episode, so reprojection replays it exactly (§5.3).
> Split is formalized as the inverse of Merge — it sets `EntityMerge.undone_at`
> rather than deleting the row — and D34 records the decision. CLI verbs in §16.4
> gain entity `alias`/`retract`, `declare`, `predicate add`, and `source policy`.
> v2.7 — Pull-connector occurrence identity (later superseded by v2.11). §4.2.1
> documented the `oxibrain sync` pull connector: source identity (`name =
> canonical vault path`, `kind = document_revision`, `mode = pull`), the
> occurrence chain `occurrence_id = H(source_id, locator, predecessor,
> content_hash)`, and the rule that legacy episodes (pre-event-path) are
> classified but never re-ingested. D33 records the decision. The v2.11
> cutover retires `oxibrain sync`; documents become a separate `documents.db`
> cache rebuilt from configured roots and gix history, with no ingest-time
> conversion into memory episodes. `Legacy document/document_revision`
> episodes in the ledger are excluded from memory search/context/extraction
> but preserved until redacted. Existing §5.6 derivation is unchanged.
> v2.9 — Daemon-hosted vault watch (ADR-010, since superseded by v2.11).
> §4.2/§16.4 once said: the daemon adopts every registered pull source into a
> debounced watcher at startup; `oxibrain sync` attached to a running daemon
> via the `sync/run` native RPC instead of failing on the P8 lock. The cutover
> in v2.11 retires the watcher, the socket-attached sync, and the durable job
> queue entirely; `oxibrain::vault` is gone. Kept here as the historical
> changelog of what v2.10 shipped before v2.11 undid it.
> **Status:** Canonical. The single source of truth for oxibrain's architecture.
> **Authority:** Superseded only by a newer dated revision of this file. Consumer projects
> (including `oxios`) adapt to this document, not the other way around.
> **Companions:** `doc/ROADMAP.md` (sequencing, exit criteria, effort), `doc/ECOSYSTEM.md`
> (how the oxi apps compose), `doc/spec/` (per-milestone implementation specs — includes
> `doc/spec/oxi-foundation-v1.md` for the cross-app Foundation contract), `doc/adr/`
>
> **Naming note:** this file was `DESIGN.md` through v1.0. Renamed because "DESIGN.md" has
> come to mean a front-end design-system document. `ARCHITECTURE.md` is the Rust-ecosystem
> convention and is unambiguous.
>
> **Convention:** `§n` refers to this document. `P n` are principles (§3), `D n` decisions
> (§23), `F n` verified findings about the current implementation (§21).

---

## 0. TL;DR

**oxibrain is a standalone, local-first knowledge and memory system** — a second brain that
stores what happened, extracts what it means, tracks how that changed over time, and answers
questions about it. It runs on its own (CLI + MCP, no oxi ecosystem required) and embeds as a
Rust library. `oxios`, `oximemo`, `oxiline`, Claude Desktop, and any other MCP client are
**consumers of equal standing**.

Everything rests on one idea:

> **An immutable ledger of episodes, and a deterministic projection derived from it.**

Everything the brain *knows* — entities, relationships, beliefs, embeddings, indexes,
rankings — is a **projection** of an append-only log. Nothing derived is precious: drop it and
recompute. Nothing recorded is silently destroyed: forgetting adjusts *salience*; destruction
is an explicit, audited **redaction**.

| Hard problem | How the ledger/projection split dissolves it |
|---|---|
| Provenance vs. forgetting | Provenance can't dangle — the ledger is never garbage-collected. Forgetting demotes salience; it does not delete truth. |
| Bad extraction | Re-extract with a better model, re-project. The ledger is unchanged; beliefs improve. |
| Wrong entity merges | Merges are data and mentions are retained → reversible by re-projecting. |
| Bi-temporality | The assertion log *is* transaction time. Belief tables cache the current slice. |
| Model / schema upgrades | A projection-version bump, not a data migration. |
| Auditability | "Why do you believe this?" is a join, always answerable. |

Three commitments shape everything else:

> **C1 — The projection is deterministic where it matters.** Same ledger, same *truth*, byte
> for byte (§5.1, P1). The ranking half gets a weaker, stated contract, because float
> embeddings are not bit-reproducible and pretending otherwise would be a lie in the test suite.
>
> **C2 — oxibrain owns its model.** Inference ships with the product. No API key wall, no
> dependency on a separate service or an agent CLI installed by the user (§8).
>
> **C3 — oxibrain is language-independent.** Not "supports many languages." Independent: no
> stemmer, no stopword list, no script branch, no language detector (§7).

C2 and C3 are one decision seen twice: owning the model means owning its tokenizer, and owning
the tokenizer is what makes a token budget exact in every writing system instead of wrong by a
factor of five in half of them (§7.5).

---
### 1.3 What ships

One engine, three delivery shapes. v2.11 cuts the resident daemon: long-lived
processes are allowed only when owned by an active caller.

| Artifact | Form | Audience |
|---|---|---|
| `oxibrain` crate | Rust library — the handle-free `Brain` facade (§3.1) | apps embedding a brain in-process |
| `oxibrain` binary | **one** executable: CLI plus caller-owned `serve --stdio` (for MCP and library sessions) and foreground `serve --http` (for the operations UI). No daemon, no socket listener, no `--watch`. | standalone users, MCP clients, a team node |
| embedded console | a **repair/operations** UI bundled inside the binary (`include_dir!`, §16.6) — no `cargo install` extra step, no Node, no `--ui-dir` | users who want to review merges, contradictions, failures, sources, and run reproject |

The product must be complete with no GUI and no ambient service:
`cargo install oxibrain-cli && oxibrain init && oxibrain ingest ~/notes && oxibrain ask "…"`.
A GUI or a daemon required for the product to make sense would mean the CLI and
stdio surfaces failed.

Under C2, model weights pull lazily on the first extraction command (§8.4); `init`
afterwards.

### 1.2 Who it is for, in order

1. **An individual** on a laptop — a personal knowledge base with an agent-native query
   surface, in whatever language they write.
2. **An agent runtime** (`oxios`, `oxicode` agents) needing durable, structured,
   temporally-aware memory instead of a text blob.
3. **A small team** sharing a self-hosted node, with per-space scoping and an audit trail.

(3) is why spaces, scoped capabilities, trust tiers, redaction, and audit are designed in
rather than bolted on: cheap now, structurally expensive later.

### 1.4 Boundary: oxibrain is not an editor

oxibrain **never owns authoring**. It does not edit markdown, manage a vault, or replace a
note app. Files belong to whatever the user writes in — `oximemo`, Obsidian, VS Code, Tolaria —
and oxibrain reads them through a connector.

```
oximemo · oxiline · oxios · Obsidian · VS Code    ← author here (owns the files)
                  ↓  vault connector (read-only, watched)
              oxibrain                             ← understands here
                  ↓  MCP · Rust API · CLI
   agents · Claude Desktop · brain UI              ← asks here
```

1. **Focus.** An editor is an enormous surface unrelated to the knowledge-graph value;
   building one means shipping a worse Obsidian.
2. **Reach.** Editor-agnosticism makes an Obsidian user a customer without changing their
   setup. Owning the editor makes them a non-customer.

One exception: **quick capture** — one input that turns a passing thought into an episode.
Capture is not authoring.

### 1.5 What "done" means for v1

- `cargo install oxibrain-cli` → ingest → ask, with **zero** oxi-ecosystem dependencies and **no
  API key**.
- Quality does not depend on the user's language (§7.8).
- Any MCP client connects and gets the full tool surface, scoped.
- `oxios` runs its entire memory subsystem on oxibrain, with no memory code of its own.
- Every answer traces to source episodes with one command.
- Published eval results as **controlled deltas** against a no-graph baseline (§17); CI blocks
  regressions.

---

## 2. Goals and non-goals

### Goals

- **Standalone first.** No required dependency on any oxi crate, any external service, or any
  API key. Ecosystem integrations are optional adapters behind feature flags.
- **Correctness over cleverness.** A confidently wrong fact is worse than a missing one. Every
  belief carries support, confidence, provenance.
- **Bi-temporal knowledge.** "When was it true" and "when did we believe it", both queryable.
- **Deterministic truth, measured ranking.** Storage, temporal logic, identity, resolution and
  belief are deterministic and property-tested. Ranking is measured, not asserted (§5.1).
- **Language-independent by construction** (§7).
- **Operable.** Migrations, backup/restore, health checks, telemetry, bounded resource use,
  crash-safe ingestion.
- **Multi-consumer safe.** Several applications share one brain without corrupting it.

### Non-goals

- An external graph database or any separate database process. Embedded SQLite only.
- A Python runtime anywhere.
- A hosted multi-tenant cloud service. A self-hosted single node is the ceiling.
- OCR / media understanding. Text in, text out; connectors may pre-transcribe.
- Multi-writer replication or consensus. Single writer; sync is post-v1 (§15.6).
- Driving the user's agent CLI as the inference path (§24).

---

## 3. Foundational principles

Invariants. Code violating one is wrong even if tests pass. Changing one requires revising this
document.

### P1 — Ledger and projection

Episodes are **immutable and append-only**. Everything else — entities, statements, beliefs,
embeddings, indexes, salience — is **derived** and reconstructible by replaying the ledger.
`oxibrain reproject` is a supported, tested operation.

P1 is scoped to the **memory plane** (`brain.db`): ledger-derived memory projections remain
replayable, while **document ranking is reconstructed from configured files and git history**
at query time. `documents.db` is a disposable cache (§5.1); dropping and rebuilding it loses
no memory — only the materialized projection of the user's files. The v2.11 cutover keeps the
ledger the single source of truth for what oxibrain *believes* and confines documents to a
rebuildable read-side cache.

The memory projection has **two halves with different contracts** (§5.1):

- **Truth** — entities, keys, merges, statements, assertions, mentions, beliefs, predicates.
  Rebuilt **byte-identically**. Anything that can change *what is believed* lives here.
- **Ranking** — vectors, FTS rows, chunks, adjacency cache, communities, salience. Rebuilt
  **equivalently**: identical membership, retrieval quality within a stated tolerance. Nothing
  here may be read by the fold.

Two corollaries with teeth:

- **The ledger is the only durable write path.** A manual `add_entity`, `add_statement`,
  `merge`, or `retract` writes a **declaration episode**, and the resulting projection rows are
  derived from it like any other. Nothing a user asserts lives only in the projection, because
  reprojection would erase it.
- **Truth is a deterministic function of the ledger.** Not "equivalent", not "isomorphic" —
  byte-identical, guaranteed by deterministic identity (§5.6) and canonical processing order.
  §17.3 tests it.

### P2 — Assertions, not facts

The system never stores "X is true". It stores "**episode E asserted, via extractor R, that X
held over interval I, with confidence c**". Truth is *computed* from the assertion set.

Corroboration, contradiction, retraction, confidence, and many-provenance become one mechanism
instead of five features.

### P3 — Identity is stable; resolution is reversible

An entity's ID is permanent and independent of its names. Names, aliases, and merges are
mutable data *about* it. Every assertion retains the **verbatim mention** it came from, so the
entity layer can be re-resolved from scratch with no model call.

A bad merge is a re-projection away from fixed, never a data-loss event.

### P4 — Semantics live in the registry, not the prompt

Predicate meaning — object type, cardinality, temporality, symmetry, inverse, invalidation — is
declared in a **versioned registry stored in the database**. One source drives five consumers:

```
PredicateRegistry ──┬──→ JSON Schema          (remote providers)
                    ├──→ GBNF grammar          (local constrained decoding, §9.4)
                    ├──→ post-extraction validator
                    ├──→ temporal upsert rules
                    └──→ generated ontology docs
```

"Does asserting `works_on(Alice, Y)` close `works_on(Alice, X)`?" is answered by data, not an
`if` buried in a pipeline.

### P5 — Forgetting is not deleting

Decay, compaction, and consolidation change **retrieval salience** and produce **derived
episodes**. They never remove ledger rows. Destruction happens only through **redaction**:
explicit, audited, cascading (§15.5).

### P6 — The engine is a library; every surface is an adapter

`oxibrain-core` knows nothing of MCP, HTTP, the CLI, or any UI. Anything reachable over MCP is
reachable in-process, and vice versa.

### P7 — Ports at the boundary

Inference, embedding, tokenization, reranking, and clock are **traits owned by oxibrain**.
still adapters — the engine choice stays reversible.

### P8 — One writer per store, scoped to one operation

Each store (`brain.db` and `documents.db` independently) admits **exactly one writing
process at a time**, enforced by an advisory lock. What changed in v2.11 is the **lifetime
of the write handle**: it belongs to **one operation, not one process**. The public `Brain`
is a handle-free runtime facade (§3.1); a read method opens its store per call and drops
the handle; a write method acquires the lock, opens a fresh writer, performs the short
database work, and drops the handle. Concurrent writers with divergent in-memory indexes
are designed out, not documented around.

Multi-application access no longer needs a daemon to arbitrate: any number of caller-owned
`serve --stdio` children (or foreground `serve --http` sessions, or in-process `Brain`
facades) can coexist because **no application monopolizes the lock**. A lock collision
retries with bounded exponential backoff ([25, 50, 100, 200, 400, 800] ms) and surfaces
`BrainError::Locked` with the holder path.

### P9 — Decision and data are separate

**Store fetches and writes. Core decides. Facade sequences.**

A function that both reads the database and chooses an outcome is a design error even when it
is correct, because it cannot be property-tested and its filters can be dropped silently.

*Evidence:* `fold` obeys this and is the best-tested component in the tree. Three executors that
did not obey it dropped `as_of` without a single failing test (F1, F3, F11).

### P10 — Compression may lose detail, never doubt

Any derived or summarized artifact carries the uncertainty computed from its support:
contradictions, single-source claims, staleness, trust exclusions. A summary is never returned
to a caller without its sources.

*Evidence:* a controlled comparison put summary-only agent memory *below* the no-memory
baseline (2.65 vs 3.30 on a 5-point scale) while summary-plus-sources scored highest (4.95),
via an overconfidence effect — clean summaries delete the traces that let a model say "I don't
know."

### P11 — No language is privileged

The system contains no stemmer, no stopword list, no script branch, and no language detector.
Lexical matching is character-n-gram based; semantic matching is multilingual-embedding based;
token counts come from the model's own tokenizer. Language-specific knowledge exists only as
registry **data** (§7.6), defaults to empty, and is never required for correctness.

*Evidence:* measured (§7.1) — a Chinese sentence produced one token, Korean tokens carried
agglutinated particles, and the token budget undercounted CJK roughly fivefold. None of that
was decided; it arrived as the default when nobody decided.

---

## 4. Architecture

### 4.1 Layers

```
┌───────────────────────────────────────────────────────────────────────┐
│ SURFACES (adapters — no business logic)                               │
│  oxibrain-cli  ·  oxibrain-mcp (stdio · foreground HTTP)              │
│  Rust API (oxibrain crate — handle-free `Brain`)                      │
│ oxibrain-views — rendered pages. Never stored, never touches SQLite   │
│  brief · navigate · profile                                           │
├───────────────────────────────────────────────────────────────────────┤
│ oxibrain-core — the engine. Pure. Decides.                            │
│  fold · resolve · rank · pack · pipeline::step · registry · identity  │
│  document planners: diff_roots · plan_reconcile · chunk_id            │
├───────────────────────────────────────────────────────────────────────┤
│ oxibrain-index — pure algorithms                                      │
│  ngram · blocking · rrf · mmr · knn · adjacency · community · quantize│
├───────────────────────────────────────────────────────────────────────┤
│ oxibrain-store — the only thing that touches SQLite. Fetches, writes. │
│  brain.db (ledger │ cache │ projection │ ops)                          │
│  documents.db (disposable document cache — own schema, own lock)       │
│  migrations · operation-scoped handle · reader pool · backup          │
├───────────────────────────────────────────────────────────────────────┤
│ oxibrain-connectors — filesystem + gix (read-only). No SQLite.         │
│  documents.toml · plain scan · gix HEAD/history/ignores               │
├───────────────────────────────────────────────────────────────────────┤
│ PORTS (traits owned by oxibrain, implementations pluggable)           │
│  LlmPort · EmbeddingPort · TokenizerPort · RerankPort · ClockPort     │
│  defaults: local GGUF · local encoder │ optional: http · MCP sampling │
└───────────────────────────────────────────────────────────────────────┘
```

The layering rule that makes this real rather than decorative is P9, and it is CI-enforced
(§18). v2.11 splits the persisted surface into two databases: `brain.db` (the memory ledger
and its projections, schema v11) and `documents.db` (a disposable cache rebuilt from
configured files and gix history). Each store has its own advisory lock and WAL; they never
share a connection.

### 4.2 Data flow

```mermaid
flowchart TB
  subgraph Memory plane ("brain.db")
    S[source: note / chat / trace / declaration] --> C[connector]
    C --> E[(episode — immutable, event-identified)]
    E --> CH[chunk + deterministic context prefix]
    CH --> X[extract: LlmPort, registry-derived grammar]
    X --> RC[(extraction cache — keyed by content+extractor)]
    RC --> V{validate against predicate registry}
    V -- invalid --> QN[(quarantine + failure record)]
    V -- valid --> M[mentions captured verbatim]
    M --> R[identity + resolution]
    R --> A[(assertions — append-only)]
    A --> B[belief projection: temporal fold]
    B --> IDX[ranking half: vectors, FTS, adjacency, communities]
  end
  subgraph Document plane ("documents.db")
    ROOT[documents.toml: alias, path, space, include/exclude] --> SC[plain-root scan]
    GIT[gix read-only: HEAD snapshot, history, ignores] --> SC
    SC --> PL[core::documents::plan_reconcile — pure]
    PL --> DC[(documents.db — doc_chunks, doc_fts_word, doc_fts_ngram, doc_vectors)]
  end
  subgraph Read
    Qy[query with planes] --> F[channels → fusion → rerank → rank — per plane]
    B --> F
    IDX --> F
    DC --> F
    F --> Ans[SearchResponse: memory hits + document hits + freshness]
    B --> BR[brief / navigate / profile]
  end
```

**Every arrow after `episode` is replayable.** Drop the projection and `reproject` rebuilds it
with no model call, because extraction outputs are cached against the ledger.

#### 4.2.1 Documents — `documents.toml`, `documents.db`, and gix read history

v2.11 retires the pull-connector (`oxibrain sync`) and the `sources` table's
document-role rows. Documents no longer become memory episodes: the v2.7 occurrence
chain (`occurrence_id = H(source_id, locator, predecessor, content_hash)`) is preserved
as a derivation rule for the legacy episodes already in the ledger, but no new document
episodes are produced. The connector surfaces below are about the new document plane.

**Document root configuration.** Document roots come only from `documents.toml` in the
brain data directory (`<dir>/documents.toml`):

```toml
[[root]]
alias = "vault"
path  = "~/.oxi/vault"            # canonicalized at load; never returned to agents
space = "personal"                  # mandatory; missing => doctor error, root disabled
include = ["**/*.md", "**/*.txt", "**/*.html"]   # default: text-bearing extensions
exclude = ["**/.git/**", "**/.DS_Store", "**/*.tmp", "**/*.lock"]
max_file_bytes = 10485760
```

Rules:

- `alias` is mandatory and globally unique (the document cache enforces this on `apply`).
- `path` is canonicalized at load but never returned to an agent.
- `space` is mandatory; a missing space disables that root with a doctor error.
- `locator` is the normalized root-relative path with `/` separators.
- symlinks are not followed; traversal outside the canonical root is rejected.
- unreadable, oversized, unsupported, and binary files are skipped and reported by
  `doctor`; they never fail a query.
- git roots additionally honor repository ignore rules through gix (§5).
- `oxibrain init` seeds `documents.toml` only when: the resolved data directory is the
  canonical default `~/.oxi/brain` and the user did not pass `--dir`; provisioning creates
  `~/.oxi/vault/<space>/` when needed (§15.1) rather than requiring a pre-existing vault.
  Explicit or temporary `--dir` never reads, writes, or references the default vault.
  Upgrade-time `open` never seeds roots. Legacy `sources` rows are never imported.
  Custom roots require an explicit config edit.

**Document identity.** For a `(root_alias, locator)` pair the document identity is

```
document_id = blake3(root_alias, locator)
revision    = git:<format>:<blob-oid>        # when a clean tracked file matches HEAD
              blake3:<hex>                    # dirty worktree / untracked / plain roots
chunk_id    = blake3(document_id, revision, ordinal)
```

`doc://<alias>/<pct-locator>?rev=<revision>` is the document URI; `DocumentRef` is a
typed literal in the predicate registry so memory declarations can reference documents
without a special case. `revision` is always bytes-shaped (no upstream coupling).

**gix read side (`GitDocumentReader`).** `oxibrain-connectors` opens repositories
read-only via `gix` (`gix::open`, fail-soft to plain-root indexing when not a repo).
HEAD is walked once per query to build `tracked: BTreeMap<locator, GitBlob>`; per-path
`is_ignored` consults `repo.excludes` with a `glob`-crate fallback. The reader provides:

- `snapshot()` — HEAD tree, object format, `tracked` map.
- `current_revision(snapshot, locator, worktree_bytes)` — `git:<format>:<oid>` when the
  worktree bytes match the HEAD blob, else `blake3:<hex>`.
- `history(alias, locator, limit)` — oldest first, filtered to commits where the path
  exists in the tree (mirrors `oxi-vault-git::log_for_file`). Powers `document_history`.
- `rename_hint(old_locator)` — latest commit where `old_locator` disappeared and a
  new path with the identical blob oid appeared; `None` on ambiguity. Doctor surfaces
  dangle.

oxibrain never initializes, adopts, commits, restores, stages, or modifies a
repository. Writes stay with oximemo/oxios via `oxi-vault-git` (ADR-011, §5.4.5).
The default build depends directly on `gix`, not on any `oxi-*` crate, preserving the
standalone guarantee.

**Legacy episode preservation.** Pull-connector episodes ingested before v2.11
(pre-event-path legacy rows with `source_id IS NULL`, plus event-path rows with
`kind = 'document' | 'document_revision'`) remain in the ledger unchanged. They are
**excluded from memory search, context assembly, and extraction** by the filter
`source_kind NOT IN ('document', 'document_revision')` — they cannot become new
memory facts. `redact` still works on them, the doc:// links still resolve through
`document_history`, and doctor reports them as a legacy section. Migration preserves
reachability.

#### 4.2.2 Curation operations

User-driven knowledge edits — **merging two entities**, **splitting a previously-merged
entity**, **adding a user-declared alias**, **registering a new predicate**, and **setting a
source ingest policy** — are first-class curation operations, not out-of-band database
fixes. Each one writes a `Declaration` episode (§5.3), so every derived row in the
projection (entity, merge, key, predicate, source policy) is reproducible from the ledger
alone. Reprojection replays declarations exactly, and the user's knowledge edits survive a
rebuild the same way user merges do (§5.3, §10.4).

The curation surface is therefore the same surface as the ingestion path: append-only,
auditable, and idempotent under reprojection. Curation never mutates a previously-written
ledger row; it appends a new one whose projection side-effects supersede what came before.
This is the same invariant P1 enforces for ingestion, restated explicitly for the curation
verbs in §16.4.

| Operation | Declaration variant | Projection effect |
|---|---|---|
| `merge`         | `Merge`              | inserts `entity_merges` row with `decided_by = User`; sets `entities.merged_into` on the loser; updates `ResolutionCache` |
| `split`         | `Split`              | sets `entity_merges.undone_at = now` on the targeted merge; clears `merged_into` for that loser; resolution re-runs only for the affected mentions (§10.4) |
| `alias`         | `Alias`              | inserts an `EntityKey` with `origin = UserDeclared`; no entity IDs change, only the name table (§10.1) |
| `retract`       | `Retract`            | sets `assertions.retracted_at = now` (P5 — P1 forbids deletion); the fold's `retracted_at IS NULL` filter (§6.6) drops it from subsequent beliefs without rewriting history |
| `declare`       | `AddStatement`       | inserts a `Declaration`-kind `Episode` plus a supporting `assertion` row — the manual equivalent of an extracted statement |
| `predicate add` | `RegisterPredicate`  | inserts a row in the predicate registry (`core/v1` seed; user predicates are additions per §5.5) — registry is **truth-half data** and minor-version changes do not invalidate the extraction cache |
| `source policy` | `SetSourcePolicy`    | inserts a `Declaration` episode carrying the new trust/policy mapping; replay rebuilds effective trust without consulting mutable external config (§15.3) |

**Split is the inverse of Merge (§10.4, D34).** Concretely: `split(merge_id)` does **not**
delete the `EntityMerge` row. It sets `undone_at` on that row and clears `merged_into` on
the previously-losing entity, so the merge remains in the ledger as a recorded past
decision. This is the same pattern P5 uses for forgetting — redaction is the only
destructive path (§15.5) — and the only way reprojection can reproduce the historical
state of the system.

**`alias` and identity (P3, §10).** `alias` only adds a `UserDeclared` entry to the
`entity_keys` table for an existing entity; it never mints a new entity id. Subsequent
extraction sees the alias in the resolver's surface candidate set, and because keys are
matched on the normalized form derived from `entity_keys.surface` (§10.1), the alias is
reproducible under reprojection. Determinism of derived IDs (§5.6) is preserved.

**`predicate add` is truth-half.** `RegisterPredicate` writes a registry row and a
`Declaration` episode. The registry is consulted by extraction prompts (§9.4) and by the
validator; because both consumers re-read the registry on every run, the new predicate
takes effect on the next extraction without code changes. Minor-version additions do **not**
invalidate the extraction cache (§5.5), so adding a predicate does not force a paid
re-extraction of existing episodes.

**Source policy is server-evaluated, not client-declared (§15.3).** `source policy` writes a
`Declaration` episode that the trust resolver replays to compute effective `TrustTier` per
source. Effective trust therefore has no mutable external dependency: replays reproduce the
same assessment because the policy lives in the ledger.

**Idempotency under reprojection** is verified by the same ledger-replay tests that cover
ingestion (§17.4) — a randomly generated ledger of `Declaration` episodes projects to the
same truth half whether replayed in one pass or incrementally built.

### 4.3 Deployment modes

v2.11 removes the resident daemon topology. The memory data plane is whatever process
currently holds the advisory lock — a CLI invocation, a `serve --stdio` child owned by
an MCP or library session, or a foreground `serve --http` operations UI. There is no
ambient discovery and no `~/.oxi/brain/oxibrain.sock` listener.

| Mode | Who runs it | Storage access | Use |
|---|---|---|---|
| **Embedded** | one host process links `oxibrain` | operation-scoped handle (P8); exclusive advisory lock only for the duration of one write | a single app, a CLI run, tests |
| **Caller-owned stdio** (`oxibrain serve --stdio --dir <dir>`) | an MCP or library session spawns the child; the child opens stores per request and dies when stdin closes | same — operation-scoped handle, no daemon hold | MCP clients, oxios/oximemo/oxiline integration, Claude Desktop |
| **Foreground HTTP** (`oxibrain serve --http <addr> --dir <dir>`) | explicit user-started UI process, loopback-only by default | operation-scoped handle | the embedded repair/operations console (§16.6) |
| **Read-only library** | any process | read-only connection, no index mutation | analytics, export |

The memory data plane is **shared infrastructure without an ambient owner**. Several
applications share one brain without a daemon because **no application monopolizes the
lock** (P8): each write takes the lock, does its short work, and releases it. C1
(ECOSYSTEM.md) holds at the data-plane level: every consuming app retains its primary
function with the brain offline; integrations degrade to a disabled panel, never to a
blocked action.

Embedded mode fails fast with `BrainError::Locked` when another process holds the
write lock, with the holder path and bounded retry guidance. Two processes with
divergent in-memory indexes writing one SQLite file is still a corruption path — the
v2.11 answer is operation-scoped handles (P8), not a topology that never existed.

---

## 5. Data model
### 5.1 Zones, and the two halves of the projection

The distinction that matters is **what it costs to lose each one**.

| Zone | Contents | Loss cost | Backup |
|---|---|---|---|
| **Ledger** | `spaces`, `episodes`, `episode_links` | irreplaceable | always |
| **Cache** | `extractions` (raw responses), `summaries` (generated text), **model weights** | rebuildable **with money and time** (weights: with bandwidth) | default yes, `--no-cache` to skip |
| **Projection — Truth** | `entities`, `entity_keys`, `entity_merges`, `statements`, `assertions`, `mentions`, `beliefs`, `predicates` | free to rebuild | default no |
| **Projection — Ranking** | vectors, FTS, `chunks`, adjacency, `communities`, salience | free to rebuild | default no |
| **Document cache** | `documents.db` — `doc_roots`, `doc_manifest`, `documents`, `doc_chunks`, `doc_fts_word`, `doc_fts_ngram`, `doc_vectors` (separate database, separate lock) | **disposable** — rebuilt from configured files and gix history by `index --documents` or any document query | default no |
| **Ops** | `extraction_failures`, `audit_log`, `meta` | audit is irreplaceable; the rest is disposable | audit always |

**The Truth/Ranking line is a contract, not a label.**

| Half | Rebuild contract | Verified by |
|---|---|---|
| **Truth** | **byte-identical** | `reproject_determinism` over `snapshot_truth` — the highest-value test in the suite, never disabled |
| **Ranking** | **equivalent**: identical membership, and retrieval recall@10 within tolerance on a fixed probe set across backends | `ranking_equivalence` over `snapshot_ranking` |

**The tolerance is calibrated, not guessed.** It is set from the measured cross-backend variance
of the shipped quantized encoder (CPU vs. Metal on the probe set), as
`max(2pp, 2 × observed_max_delta)`, and recorded here with its measurement. A number invented
before measurement is a guess; the same mistake §17.2 calls out for quality targets.

| Field | Value |
|---|---|
| Measured | 2026-08-13, Apple M4 · `bge-m3-Q4_K_M.gguf` |
| Probe set | `eval/probes/probes.toml` — 39 entities × 20 queries across Latin, Hangul, Han, Kana, Arabic, Thai (§7.8) |
| Recall@10, CPU (`n_gpu_layers=0`) | **1.0000** (10 runs) |
| Recall@10, Metal (`n_gpu_layers=all`) | **1.0000** (10 runs) |
| Observed max delta | **0.00pp** |
| **Tolerance** | `max(2pp, 2 × 0.00pp)` = **2pp** |

Runner: `crates/oxibrain-embed-local/tests/ranking_equivalence.rs`
(`cargo test -p oxibrain-embed-local --test ranking_equivalence -- --ignored`). The floor of 2pp
applies because the two backends are indistinguishable on this probe set — the delta is exactly
zero, so the tolerance is the `2pp` floor, not a multiple of a nonzero variance.

Model weights are Cache-zone (§8.4): expensive to reproduce, cheap to re-fetch, never
irreplaceable.

### 5.2 Ledger types

```rust
/// A namespace and isolation boundary. All queries are space-scoped.
pub struct Space { pub id: SpaceId, pub name: String, pub created_at: Timestamp }

/// The atom of record. Immutable once written.
pub struct Episode {
    pub id: EpisodeId,              // event-derived for new-path episodes (§5.6)
    pub space: SpaceId,
    pub seq: u64,                   // monotonic ingest order; defines canonical replay order
    pub content_hash: ContentHash,  // BLAKE3 integrity verification; not identity
    pub content: String,
    pub source: SourceRef,
    pub trust: TrustTier,           // server-evaluated (§15.3)
    pub kind: EpisodeKind,          // Primary | Declaration | Derived     (§5.3)
    pub occurred_at: Timestamp,     // when it happened in the world
    pub ingested_at: Timestamp,     // when the system received it
    pub redacted_at: Option<Timestamp>,
}
```

`SourceRef` — `Conversation | Note{path} | Document{uri} | DocumentRevision{uri} |
ArtifactEvent{uri} | WebClip{uri} | CalendarEvent{uri} | Message | AgentTrace | Declaration |
Derived{of}`.

### 5.3 Episode kinds — closing the derived-episode loop

| Kind | Written by | Re-extracted? | Determinism |
|---|---|---|---|
| `Primary` | connectors, `ingest` | yes | content comes from outside; deterministic input |
| `Declaration` | manual API/CLI/MCP writes | **no** — it carries structured claims, not prose | fully deterministic |
| `Derived` | consolidation, community summaries | **never** | generated text cached in `summaries`, keyed by `(kind, member_set_hash, extractor_id)`; reprojection reuses the cache and regenerates only on `--regenerate-summaries` |

Derived episodes are searchable, quotable, and provenance-carrying, but **terminal** — no
assertion is ever extracted from one, so the feedback loop cannot exist. Every derived episode
additionally carries a computed `Uncertainty` block (§13.1, P10).

`Declaration` episodes are what make P1's first corollary work. Reprojection replays them
exactly, so user knowledge and user merges survive a rebuild.

### 5.4 Knowledge types

```rust
/// Opaque, permanent identity. Names live in `EntityKey`, not here (P3).
pub struct Entity {
    pub id: EntityId,               // derived (§5.6), stable across renames
    pub space: SpaceId,
    pub ty: EntityTypeRef,
    pub canonical_key: Option<EntityKeyId>,
    pub created_at: Timestamp,
    pub merged_into: Option<EntityId>,   // redirect; lookups follow the chain
}

/// A (type, normalized name) handle. Aliases are additional keys on one entity.
pub struct EntityKey {
    pub id: EntityKeyId,
    pub space: SpaceId,
    pub entity: EntityId,
    pub ty: EntityTypeRef,
    pub normalized: String,         // NFKC + casefold + whitespace collapse (§7.6)
    pub surface: String,            // as written
    pub origin: KeyOrigin,          // Extracted | UserDeclared | Imported
}

/// Merges are data, so they replay and reverse (P3).
pub struct EntityMerge {
    pub loser: EntityId,
    pub winner: EntityId,
    pub decided_by: MergeDecision,  // Rule{score} | User | Import
    pub provenance: EpisodeId,      // a Declaration episode for user merges
    pub evidence: Vec<MentionId>,
    pub decided_at: Timestamp,
    pub undone_at: Option<Timestamp>,
}

/// An atemporal proposition. Content-addressed → deduplicated by construction.
pub struct Statement {
    pub id: StatementId,            // hash(space, subject, predicate, object)
    pub space: SpaceId,
    pub subject: EntityId,
    pub predicate: PredicateRef,
    pub object: Object,             // Entity(EntityId) | Literal(TypedValue)
}

/// "Episode E, via extractor R, claimed S held over I, with confidence c."
pub struct Assertion {
    pub id: AssertionId,
    pub statement: StatementId,
    pub episode: EpisodeId,         // provenance — mandatory, FK-enforced
    pub extractor: Option<ExtractorId>,  // None = manual declaration
    pub polarity: Polarity,         // Affirm | Deny
    pub claimed_from: Timestamp,    // TIME_MIN when unbounded (§6.2)
    pub claimed_to: Timestamp,      // TIME_MAX when still true
    pub confidence: f32,
    pub recorded_at: Timestamp,     // transaction time
    pub retracted_at: Option<Timestamp>,
}

/// The verbatim text this assertion came from — the key to reversible resolution.
pub struct Mention {
    pub id: MentionId,
    pub assertion: AssertionId,
    pub role: MentionRole,          // Subject | Object
    pub surface: String,            // verbatim, as it appeared
    pub span: (u32, u32),           // byte offsets into the episode
    pub resolved_to: Option<EntityId>,
    pub method: ResolutionMethod,   // ExactKey | Alias | Lexical{score} | Embedding{score} | New | User
}

/// Current-slice cache of the temporal fold. Fully derived.
pub struct Belief {
    pub statement: StatementId,
    pub valid_from: Timestamp,      // NOT NULL — sentinel, never NULL (§6.2)
    pub valid_to: Timestamp,
    pub support: Support,           // affirm/deny counts, distinct episodes, trust mix
    pub confidence: f32,            // computed (§6.5)
    pub status: BeliefStatus,       // Active | Superseded | Contradicted | Retracted
}
```

There is intentionally **no `Fact` type** and no physical `Relation` / `Observation`. Those are
API renderings:

| API view | Physical | Rationale |
|---|---|---|
| `Relation` | `Statement` where `object = Entity(_)` | keeps MCP / `mcp-knowledge-graph` vocabulary |
| `Observation` | `Statement` where `object = Literal(_)` | same |
| `Fact` | `Statement` + `Belief` + top provenance | what a caller actually wants back |

### 5.5 Predicate registry (P4)

```rust
pub struct PredicateDef {
    pub name: String,                   // "works_on"
    pub object_kind: ObjectKind,        // Entity(EntityTypeRef) | Literal(LiteralType) | Enum(Vec<String>)
    pub subject_types: Vec<EntityTypeRef>,
    pub cardinality: Cardinality,       // Functional | MultiValued
    pub temporality: Temporality,       // Static | Interval | Point
    pub invalidation: Invalidation,     // Supersede | Coexist | ExplicitOnly
    pub symmetric: bool,
    pub inverse_of: Option<String>,
    pub profile_relevant: bool,         // §12.2 — belongs in the standing profile query
    pub description: String,            // fed verbatim into the extraction prompt
    pub examples: Vec<String>,          // few-shot material (§9.6)
    pub deprecated_by: Option<String>,
}

pub struct EntityTypeDef {
    pub name: String,
    pub description: String,
    pub strip_affixes: Vec<String>,     // §7.6 — data, never code. Defaults to empty.
}
```

`LiteralType` is typed, not free JSON: `Text | Date | DateTime | Quantity{unit} | Number |
Bool | Enum`. Dates especially must be typed or `timeline` degrades to string comparison.

**Shipping ontology `core/v1`** — entity types `Person, Organization, Project, Concept, Place,
Event, Artifact, Document, Code, Task`; ~40 predicates, plus a small **negative/uncertain**
family (`failed_because`, `assumed`, `unknown_whether`) so agent traces can record what did not
work and what is not known. It is **data seeded by migration**, so projects extend it
(`oxibrain predicate add`) without forking code.

**Versioning.** `ExtractorId` includes the registry version (§9.5), so a naive bump invalidates
every cached extraction and forces a full, paid re-extraction. Therefore:

- **Major** — changing or removing an existing predicate's semantics. Invalidates the cache.
- **Minor** — adding a predicate, an entity type, a `profile_relevant` flag, or an affix list.
  **Does not** invalidate.

`ExtractorId` hashes the registry **major** version only.

### 5.6 Deterministic identity

Every truth-half ID is **deterministically derived**, and the derivation is acyclic. For episodes
written through the event path, identity is the registered source occurrence within a space:

```
EpisodeId    = blake3(space_id, source_id, occurrence_id)
EntityId     = blake3(space, entity_type, first_episode_id, first_span_start)
EntityKeyId  = blake3(entity_id, normalized, ty)
StatementId  = blake3(space, subject_entity_id, predicate, object_repr)
AssertionId  = blake3(statement_id, episode_id, extractor_id, claim_repr)
MentionId    = blake3(assertion_id, role, span)
ChunkId      = blake3(episode_id, ordinal)
```

The tuple `(space_id, source_id, occurrence_id)` is the episode identity for new-path episodes.
`content_hash` verifies the payload bytes; it is not identity. Equal bytes from independent
sources therefore remain independent episodes. Reusing an occurrence with the same bytes is an
idempotent retry; reusing it with different bytes is a conflict. Legacy internal callers retain
their content-hash-deduplicated write path while they migrate to event identity.

No cycle: entities are keyed by *where they were first mentioned* — a location in an immutable,
event-identified episode — not by anything downstream of themselves. A rename or a merge never
changes an `EntityId`, so P3 holds.

"First mention" is well defined because replay has a **canonical order**:
`(episode.seq, extractor_id, statement_index_within_response)`. Incremental ingestion follows
the same order by construction. Reprojection therefore reproduces the incremental result
exactly.

`object_repr` and `claim_repr` are canonical serializations: sorted keys, normalized numbers,
RFC-3339 UTC timestamps. Canonicalization is a single shared function with its own property
tests.

### 5.7 Schema sketch

```sql
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;

-- ── Ledger (schema v10) ───────────────────────────────────────────────
CREATE TABLE spaces (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL
);

CREATE TABLE sources (
  id TEXT PRIMARY KEY, space_id TEXT NOT NULL REFERENCES spaces(id),
  name TEXT NOT NULL, kind TEXT NOT NULL, mode TEXT NOT NULL,
  claims_json TEXT NOT NULL DEFAULT '{}', created_at INTEGER NOT NULL,
  UNIQUE (space_id, name)
);

CREATE TABLE episodes (
  id           TEXT PRIMARY KEY,
  space_id     TEXT NOT NULL REFERENCES spaces(id),
  seq          INTEGER NOT NULL,
  content_hash BLOB NOT NULL,
  content      TEXT NOT NULL,
  source_kind  TEXT NOT NULL,
  source_ref   TEXT,
  trust        TEXT NOT NULL,
  kind         TEXT NOT NULL,
  occurred_at  INTEGER NOT NULL,
  ingested_at  INTEGER NOT NULL,
  redacted_at  INTEGER,
  source_id    TEXT REFERENCES sources(id),
  occurrence_id TEXT,
  accepted_at  INTEGER,
  principal    TEXT,
  claims_json  TEXT,
  UNIQUE (space_id, seq)
);
CREATE UNIQUE INDEX idx_ep_occurrence
  ON episodes(space_id, source_id, occurrence_id)
  WHERE source_id IS NOT NULL AND occurrence_id IS NOT NULL;

-- Schema v10 deliberately has no UNIQUE(space_id, content_hash): equal bytes
-- from independent source occurrences are independent episodes.

CREATE TABLE source_policies (
  id             TEXT PRIMARY KEY,
  source_id      TEXT NOT NULL REFERENCES sources(id),
  trust          TEXT NOT NULL,
  effective_from INTEGER NOT NULL,
  effective_to   INTEGER,
  declaration_ep TEXT NOT NULL REFERENCES episodes(id),
  created_at     INTEGER NOT NULL
);

CREATE TABLE episode_links (
  from_episode TEXT NOT NULL REFERENCES episodes(id),
  to_episode   TEXT NOT NULL REFERENCES episodes(id),
  rel          TEXT NOT NULL,             -- summarizes | revises | replies_to
  PRIMARY KEY (from_episode, to_episode, rel)
);

-- ── Cache ─────────────────────────────────────────────────────────────
CREATE TABLE extractions (
  episode_id    TEXT NOT NULL REFERENCES episodes(id),
  extractor_id  TEXT NOT NULL,
  response_hash BLOB NOT NULL,
  raw_response  TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (episode_id, extractor_id)
);

CREATE TABLE summaries (
  scope_kind      TEXT NOT NULL,          -- consolidation | community
  member_set_hash BLOB NOT NULL,
  extractor_id    TEXT NOT NULL,
  text            TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (scope_kind, member_set_hash, extractor_id)
);

-- ── Projection: Truth ─────────────────────────────────────────────────
CREATE TABLE entities (
  id            TEXT PRIMARY KEY,
  space_id      TEXT NOT NULL REFERENCES spaces(id),
  type_name     TEXT NOT NULL,
  canonical_key TEXT REFERENCES entity_keys(id) DEFERRABLE INITIALLY DEFERRED,
  created_at    INTEGER NOT NULL,
  merged_into   TEXT REFERENCES entities(id)
);

CREATE TABLE entity_keys (
  id         TEXT PRIMARY KEY,
  space_id   TEXT NOT NULL REFERENCES spaces(id),
  entity_id  TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
  type_name  TEXT NOT NULL,
  normalized TEXT NOT NULL,
  surface    TEXT NOT NULL,
  origin     TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_entity_key_unique ON entity_keys(space_id, type_name, normalized);
CREATE INDEX idx_entity_key_entity ON entity_keys(entity_id);

CREATE TABLE statements (
  id             TEXT PRIMARY KEY,
  space_id       TEXT NOT NULL REFERENCES spaces(id),
  subject_id     TEXT NOT NULL REFERENCES entities(id),
  predicate      TEXT NOT NULL,
  object_entity  TEXT REFERENCES entities(id),
  object_literal TEXT,
  CHECK ((object_entity IS NULL) != (object_literal IS NULL))
);

CREATE TABLE assertions (
  id           TEXT PRIMARY KEY,
  statement_id TEXT NOT NULL REFERENCES statements(id) ON DELETE CASCADE,
  episode_id   TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT,
  polarity     INTEGER NOT NULL,
  claimed_from INTEGER NOT NULL,
  claimed_to   INTEGER NOT NULL,
  confidence   REAL NOT NULL,
  recorded_at  INTEGER NOT NULL,
  retracted_at INTEGER,
  trust        TEXT NOT NULL DEFAULT 'trusted'
);

CREATE TABLE beliefs (
  statement_id TEXT NOT NULL REFERENCES statements(id) ON DELETE CASCADE,
  valid_from   INTEGER NOT NULL,          -- NOT NULL: PK correctness
  valid_to     INTEGER NOT NULL,
  status       TEXT NOT NULL,
  confidence   REAL NOT NULL,
  support_json TEXT NOT NULL,
  PRIMARY KEY (statement_id, valid_from)
);

-- ── Projection: Ranking ───────────────────────────────────────────────
CREATE TABLE chunks (
  id         TEXT PRIMARY KEY,            -- blake3(episode_id, ordinal)
  space_id   TEXT NOT NULL REFERENCES spaces(id),
  episode_id TEXT NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  span_start INTEGER NOT NULL,            -- byte offsets into episodes.content
  span_end   INTEGER NOT NULL,
  context    TEXT NOT NULL,               -- deterministic prefix, §9.3
  UNIQUE (episode_id, ordinal)
);

-- Two lexical indexes, both always populated. No script routing (§7.4).
CREATE VIRTUAL TABLE fts_word  USING fts5(body, space_id UNINDEXED,
    target_kind UNINDEXED, target_id UNINDEXED, tokenize = 'unicode61');
CREATE VIRTUAL TABLE fts_ngram USING fts5(body, space_id UNINDEXED,
    target_kind UNINDEXED, target_id UNINDEXED, tokenize = 'trigram');

-- Dense vectors via sqlite-vec; quantized (D25).
-- communities, adjacency cache, salience: see §11.

-- ── Ops (schema v11) ───────────────────────────────────────────────────
-- The extraction backlog is now `uncached_memory_episodes(...)` (§9.1); the
-- durable `ingest_jobs` queue was dropped in v11. The extraction cache and the
-- audit log remain:
CREATE TABLE extraction_failures (
  episode_id   TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT NOT NULL,
  raw_response TEXT NOT NULL,
  error        TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (episode_id, extractor_id)
);
CREATE TABLE audit_log (
  id          TEXT PRIMARY KEY,
  actor       TEXT NOT NULL,
  scope       TEXT NOT NULL,
  operation   TEXT NOT NULL,
  target      TEXT NOT NULL,
  recorded_at INTEGER NOT NULL
);

Chunk text is **not** stored — it is `substr(episodes.content, span_start, …)`. One copy of the
bytes, and redaction already tombstones the source.

`ON DELETE CASCADE` appears only within the projection. No cascade crosses from the ledger into
knowledge; redaction runs its own explicit procedure (§15.5).

### 5.8 Migrations

- `PRAGMA user_version` as the counter; ordered, forward-only, embedded `.sql` plus optional
  Rust steps.
- Every migration has an up-test against a fixture database of the previous version; CI runs
  the full chain from v1.
- Two version numbers in `meta`: `ledger_schema_version` (migrated carefully) and
  `projection_version` (bumping it triggers a **rebuild**, not a migration — P1's dividend).
- Opening a store newer than the binary is a hard error naming the required version.

---

## 6. Temporal and belief semantics

### 6.1 Two time axes

| Axis | Where it lives | Question |
|---|---|---|
| **Valid time** | `assertions.claimed_from/_to` → `beliefs.valid_from/_to` | "Was X true on 2024-03-01?" |
| **Transaction time** | `assertions.recorded_at / retracted_at` | "What did the brain believe on 2024-03-01?" |

Transaction time needs no history table: **the assertion log is the transaction-time record.**
Belief as of transaction time *S* is the fold over assertions with `recorded_at <= S AND
(retracted_at IS NULL OR retracted_at > S)`. `beliefs` caches only `S = now`.

Both axes are queryable through `Filters { as_of, known_at }` (§11.2) — not only through
`beliefs_as_of`.

### 6.2 Sentinel time, not `Option<Timestamp>`

Open intervals use `TIME_MIN` / `TIME_MAX` (`i64::MIN + 1` / `i64::MAX - 1`) rather than NULL.

1. **Primary-key correctness** — SQLite permits NULLs in a `PRIMARY KEY` column.
2. **Interval algebra without special cases** — every comparison is a plain integer comparison.
3. **Index usability** — a range scan works; `IS NULL OR` does not.

### 6.3 The fold

For one statement, in `recorded_at` order:

1. Filter to assertions visible at *S*.
2. Partition by polarity. Denials clip affirming intervals; a denial with no interval clips
   from its `recorded_at`.
3. Merge overlapping/adjacent affirming intervals into disjoint belief intervals.
4. Apply the predicate's cardinality and invalidation (§6.4).
5. Compute confidence (§6.5) and status.

```rust
fn fold(def: &PredicateDef, assertions: &[Assertion], at: Timestamp,
        calibration: &CalibrationTable) -> Vec<Belief>
```

A pure function, therefore property-testable, and it is: *intervals are disjoint and ordered*,
*retraction is monotone*, *fold(prefix) then fold(suffix) equals fold(whole)*, *fold is
idempotent* — all proptest properties gating CI.

**`fold` is the archetype for P9.** Store fetches the group; core decides; store applies. Every
other decision point in the system should look like this.

### 6.4 Invalidation is declared, never guessed

| `cardinality` | `invalidation` | New assertion on the same subject+predicate |
|---|---|---|
| `Functional` | `Supersede` | closes the previous interval at the new `claimed_from` |
| `MultiValued` | `Coexist` | adds a parallel interval; nothing closes |
| any | `ExplicitOnly` | only an explicit `Deny` closes an interval |

`employed_by` is `Functional/Supersede`. `works_on` is `MultiValued/Coexist`. `born_in` is
`Static`: a second value raises a **contradiction**, not a supersession.

Contradiction is a first-class outcome. `BeliefStatus::Contradicted` keeps both intervals,
surfaces both provenances, and downranks the statement until resolved. **The system never
silently picks a winner.** An LLM never holds the delete key (D15).

### 6.5 Confidence

```
confidence = calibrate(extractor) · corroboration · trust · recency_of_support
```

- `calibrate(extractor)` — per-extractor multiplier measured by the eval harness (§17). An
  unmeasured extractor gets a conservative prior of 0.8. Under C2 there are normally at least
  two extractors in play (local tier 0, optional tier 1), and calibration is what lets the fold
  weigh them correctly without a hand-tuned rule.
- `corroboration` — saturating in the count of **distinct episodes**. Ten assertions from one
  episode are one episode's evidence.
- `trust` — weighted by the trust tier of supporting episodes (§15.3).
- `recency_of_support` — `Interval` predicates only.

Manual declarations bypass this at `1.0`.

### 6.6 Temporal queries

- `timeline(entity, [range])` — belief intervals touching the range, with change points.
- `as_of(query, valid_time, [transaction_time])` — any read pinned on either axis, including
  **traversal** (§11.5).
- `diff(t1, t2)` — what the brain learned, changed its mind about, or forgot between two
  transaction times.

---

## 7. Language independence

> **C3.** No language tables, no stemmer, no stopword list, no script branch, no detector.

### 7.1 The measurement that forced this section

The v1.0 tokenizer, compiled verbatim and run over seven writing systems:

```
en  n=3  ["alice", "works", "projectx"]
ko  n=3  ["김민수는", "프로젝트x에서", "일한다"]
ja  n=1  ["田中さんはプロジェクトxで働いています"]
zh  n=1  ["张伟在项目x工作"]
es  n=5  ["alice", "trabaja", "en", "el", "proyectox"]
th  n=2  ["ผมทำงานท\u{e35}", "โครงการx"]
ar  n=4  ["يعمل", "علي", "في", "المشروع"]
```

- **Chinese and Japanese sentences become one token.** Lexical retrieval degenerates to
  whole-sentence exact match. Not weak — absent.
- **Korean tokens carry their particles.** `김민수는` is name + topic marker; a search for
  `김민수` never matches it, and the inflected form is the normal form.
- **Thai loses a combining character.**
- **Spanish keeps its function words**, because the stopword list is English.
- The `s.len() > 1` filter operates on **bytes**, so it drops one-letter Latin tokens and keeps
  one-character CJK ones.

FTS was configured `tokenize = 'porter unicode61'` — the Porter *English* stemmer plus a
tokenizer that splits on word boundaries that do not exist in Chinese, Japanese or Thai.

None of this was decided. It arrived as the default when nobody decided, which is exactly how
language dependence always arrives.

### 7.2 Never detect the language

The obvious fix — a detector routing to per-language tokenizers — is rejected:

1. **Detection fails where it matters**: short text, mixed-script text, code, names. A personal
   knowledge base is full of all four.
2. **It creates an open-ended maintenance surface.** Every new user language is a ticket.
3. **It makes behaviour discontinuous.** Two similar notes take different code paths, and
   determinism becomes conditional on a heuristic.

Instead, pick primitives that never needed to know the language:

> **Character n-grams for anything lexical. Multilingual embeddings for anything semantic.
> Rank fusion to decide which was right.**

The lexical channel does not need to be smart — it needs to be **exact and universal**;
semantics are the embedding's job. That division makes language independence structural rather
than a support matrix.

### 7.3 One primitive, four uses

| Use | v1.0 | Now |
|---|---|---|
| Full-text index | `porter unicode61` | `unicode61` **and** `trigram` (§7.4) |
| Fallback vectors | English word tokenizer | character n-grams |
| Fuzzy name similarity | `jaro_winkler` | n-gram Jaccard (§7.5) |
| Resolution blocking | *(absent)* | MinHash/LSH over the same shingles (§10.1) |

The last row is the payoff: the blocking index that §10.1 needs is **free** once the similarity
primitive is n-gram-based, because it reuses the shingles the other three rows already compute.

### 7.4 Two lexical channels, fused rather than routed

The standard CJK recipe routes by script: `unicode61` for space-separated text, `trigram` for
CJK. Routing is a detector wearing a hat.

We do not need it, because rank fusion already exists. **Index both. Query both. Let RRF
decide.** On English the word index dominates because BM25 ranks cleanly over real tokens; on
Japanese it contributes nothing and the n-gram index carries the channel; on mixed text both
contribute. No detection, no branch, no per-language configuration — and `explain` reports
which channel produced each hit, so the behaviour is inspectable rather than magical.

Two costs, stated:

- **Index size.** Trigram indexes are larger. Bounded by indexing *chunks* rather than whole
  episodes. Measured in M7; if the ratio exceeds 3× the word index, the mitigation is
  chunk-level-only n-gram indexing, not script routing.
- **Sub-trigram tokens.** Trigram cannot index 1–2 character tokens, which are common and
  meaningful in CJK. A bounded `LIKE '%…%'` scan over the candidate set contributes as a third
  channel for short queries only.

### 7.5 Exact token budgets — where C2 and C3 meet

v1.0 estimated tokens as `chars / 4`, an English heuristic. In Chinese and Japanese a character
is roughly one token or more, so a 3,000-token budget could emit around 15,000 real tokens.

That is not a quality regression. It is a **context-window overflow** — the caller's request
fails, in one class of languages, for a reason nothing reports.

Every heuristic fix is a language table, and §7.2 rejects those. The correct fix is to **count
with the real tokenizer**:

```rust
pub trait TokenizerPort: Send + Sync {
    fn count(&self, text: &str) -> usize;
    fn truncate_to<'a>(&self, text: &'a str, max: usize) -> &'a str;
}
```

Under C2 we ship the model, so we ship its tokenizer, so this is exact in every language at no
extra cost. **Owning the model is what makes the budget honest.**

`estimate_tokens_rough` survives only as the pre-load fallback, named so no caller mistakes it
for a measurement.

### 7.6 Normalization and affixes

- `normalize()` is NFKC + Unicode full casefold + whitespace collapse. All three are
  script-neutral; none privileges a language.
- **Affix stripping is registry data** (`EntityTypeDef::strip_affixes`, §5.5), seeded with a
  small multilingual set and extensible by users. If empty, normalization still works — affix
  stripping is a precision improvement, never a correctness requirement. **No language is
  privileged by being compiled into the binary.**

### 7.7 Name similarity without the prefix bonus

Jaro-Winkler adds a bonus for shared prefixes, because it was designed for Western
given-name-first records where a shared prefix is evidence. **In surname-first orders a shared
prefix is the surname.** On Korean, Chinese, Japanese, Hungarian and Vietnamese names, Winkler
systematically scores `김민수` and `김서연` as more similar — and §10 cites "two people named
Kim" as the motivating case for context overlap. v1.0 was making its own hardest case harder.

Replacement: **n-gram Jaccard over the normalized surface** — order-insensitive, prefix-neutral,
identical in every script, sharing shingles with §7.3. Positional evidence comes from **graph
context** (§10.2) — evidence about the world, not a spelling heuristic.

### 7.8 Testing by writing-system property, not by language

A language list is always incomplete. Organize the parity corpus by the **properties that break
implementations**:

| Property | Representative | Breaks |
|---|---|---|
| No word delimiters | zh, ja, th | word tokenizers |
| Agglutinative morphology | ko, tr, fi | stem matching |
| Right-to-left | ar, he | span offsets, rendering |
| Combining marks / normalization | th, vi, hi | NFKC assumptions |
| No letter case | zh, ja, ar, ko | casefold-dependent logic |
| Surname-first names | ko, zh, ja, hu | prefix-bonus similarity |
| Latin baseline | en, es | the control |

Seven representatives cover all seven properties. The CI gate is a **variance bound**:

> **No retrieval or resolution metric may vary by more than 10 percentage points across
> property classes on the parity corpus.**

That gate is the executable form of C3. It fails when someone adds an English-shaped
optimization, and it keeps passing for languages we never tested — because it tests properties,
not languages.

---

## 8. The model

> **C2.** Inference ships with the product.

### 8.1 What "embedded" means, precisely

| Property | Committed |
|---|---|
| Works with no API key | **yes** |
| Works with no network, after `init` | **yes** |
| Works with no other software installed (no Ollama, no agent CLI) | **yes** |
| Ships weights inside the executable | **no** — §8.4 |
| Precludes using a frontier provider | **no** — it is the default, not the ceiling (§8.5) |

Row 4 matters: a `cargo install` binary cannot carry gigabytes. "Embedded" is a **product**
property — no external service, no account, no second install — not a statement about linkage.

### 8.2 Ports, and which adapter is the default

`LlmPort` and `EmbeddingPort` predate C2. C2 does not change the architecture; it changes which
adapter is the default. The engine choice stays reversible, which is the same argument D12
makes for SQLite.

| Crate | Provides | Default |
|---|---|---|
| `oxibrain-llm-local` | `LlmPort` + `TokenizerPort` — GGUF, CPU/Metal/CUDA | **yes** |
| `oxibrain-embed-local` | `EmbeddingPort` — multilingual encoder | **yes** |
| `oxibrain-llm-http` | `LlmPort` — Anthropic / OpenAI / Ollama | feature |
| `oxibrain-embed-http` | `EmbeddingPort` — hosted encoders | feature |
| `oxibrain-mcp` sampling | `LlmPort` — the client's model | capability, off by default |

`TokenizerPort` ships **with** `oxibrain-llm-local` rather than as its own crate: it is the
model's own tokenizer, has the model's lifecycle, and a crate per trait impl is over-crating.

`oxibrain-embed-local` stays separate from `oxibrain-llm-local` because the two have different
model lifecycles and because retrieval-only deployments need embeddings without inference.

**Engine.** `llama.cpp` via `llama-cpp-2` is the recommended backing, for one decisive reason:
**GBNF grammar-constrained decoding** (§9.4), which pure-Rust stacks do not yet match and which
is what makes a small model usable for extraction. Its cost is a bundled C++ build — and we
already bundle C for SQLite via `rusqlite/bundled`, so the machinery and cross-compilation
story exist. `candle` and `mistral.rs` are re-examined at the M7 gate; because this is an
adapter, switching is a crate swap.

**Model selection is config, not code.** The architecture fixes requirements:

| Role | Requirement |
|---|---|
| Extraction | multilingual instruct model, ≈1–3 GB quantized, grammar-constrained decoding, ≥8K context |
| Embedding | multilingual encoder, ≤1 GB quantized, ≥100 languages, truncatable dimensions preferred |
| Reranking | optional small multilingual cross-encoder |

Naming a checkpoint here guarantees the document is stale within a year. The eval harness picks
it (§17); the digest pins it (§8.4).

**Multilingual is not negotiable under C3.** An English-first encoder makes the default
configuration bad in most of the world.

### 8.3 The `Sample` capability, revisited

D14 adopted MCP client sampling because "requiring an API key before their notes mean anything
is the largest onboarding drop-off in a local-first tool that needs an LLM."

**C2 removes that wall entirely**, so sampling is no longer the onboarding answer. Its remaining
value is quality (tier 1′, §8.5).

The conclusion is unchanged and the reasoning is stronger: sampling routes episode content
through a third-party client's provider, so it remains a **separate capability, off by default,
granted per token and per space, and audited.** It was previously a privacy cost accepted for
onboarding; it is now one accepted only for quality, on episodes the user's policy selects.

### 8.4 Model artifacts are Cache-zone

Weights are fetched lazily — by the first command that needs them (`extract`,
`reextract`) — into `~/.oxi/models/`, or `$OXIBRAIN_MODELS_DIR` when set, pinned
by digest. `oxibrain init` provisions the store and nothing else: the empty
install is instant, and exploration commands (`stats`, `page`, MCP read tools)
never download. This is the **Cache zone** the design already defines: expensive
to reproduce, cheap to re-fetch, never irreplaceable.

- The digest is an input to `ExtractorId`, so swapping weights is a normal re-extraction, never
  a silent quality change.
- `oxibrain doctor` verifies digests and reports drift.
- An air-gapped install points `OXIBRAIN_MODELS_DIR` at a pre-pulled directory — no special
  build, and the lazy pull becomes a verify-only no-op.
- `oxibrain backup --no-cache` skips weights; restore re-fetches.

The first extraction shows download progress and is resumable (`<file>.part`,
HTTP Range). That command is the moment the user has committed to the workflow,
so the one-time download lands there, not in `init` (ADR-005).

### 8.5 Tiering: local first, escalate deliberately

"Cheap first pass, expensive second pass" is expressible precisely because re-extraction has no
side effects (§9.7). C2 makes the cheap tier free:

| Tier | Extractor | When |
|---|---|---|
| **0** | local model, grammar-constrained | every episode, always |
| **1** | frontier provider | high-salience episodes, if configured |
| **1′** | MCP client sampling | same, when a live session grants `Sample` |

Both tiers write assertions; the fold weighs them by calibrated confidence (§6.5); `oxibrain
why` shows which extractor claimed what. **A user with no API key gets a complete product; a
user with one gets a better graph on the episodes that matter.** No feature is gated, only
quality — the right shape for a local-first tool.

### 8.6 The Foundation boundary — what oxibrain owns and what it does not

C2 names a default and a ceiling; the Foundation v1 contract (ADR-007,
`doc/spec/oxi-foundation-v1.md`) names where profile resolution happens. The default
`LlmPort` implementation remains the local GGUF adapter (`oxibrain-llm-local`). Remote
adapters — Anthropic, OpenAI, Ollama, hosted encoders — are **profile-selected**, not
process-wide: a host may swap in a Foundation profile whose provider is a frontier endpoint,
but only at the facade/CLI boundary (`Brain::with_llm` and the `oxibrain` CLI's
`--profile <id>`), with the role-binding and Keychain-locator rules enforced by the
adapter before any secret is read.

Two things are explicitly out of scope for oxibrain, and the Foundation contract does not
change that:

1. **No driving of another app's CLI for inference.** The Oxicode TUI/CLI is an
   application surface, not an inference backend. `oxibrain` never invokes
   `oxicode ...` to satisfy an `LlmPort` call. A profile's `provider` field refers to an
   inference provider the user has configured locally (HTTP endpoint, MCP-sampling client),
   not to another oxi binary.
2. **No mandatory model gateway.** oxibrain does not require a shared inference
   daemon, a Foundation-side router, or a Foundation-side load balancer. Two hosts may
   hold the same profile file and select different engines at the facade; that is the
   point of profile selection. A profile is a **policy document**, not a routing key.

---

## 9. Ingestion and extraction

### 9.1 Stages

v2.11 removes the durable extraction job queue. The backlog is now a derived query —
`uncached_memory_episodes(space, extractor_id)` in `oxibrain-store` — that returns the
set of memory-plane episodes with no extraction row for this extractor, in `seq` order.
`extract_uncached(limit)` walks it under the write lock; `pending_extraction_stats()`
reports `{count, oldest_seq}` without walking the whole ledger. `remember` / explicit
capture / `ingest` append the episode inline and return `Captured { episode_id }` or
`CapturedPending { episode_id, pending_count }` when the model call fails; the caller
retries via `extract_uncached`.

```
connector → episode (idempotent by event identity)
  → chunk (+ deterministic context prefix)
  → extract (LlmPort, registry-derived grammar) → cache raw response
  → parse → validate against registry → capture mentions → resolve identity
  → write assertions → fold beliefs → update ranking half
```

Model calls happen between writes, never inside a write transaction (§9.2). The
`Compute(Effect)` step is outside any `StoreHandle`; the `Commit(WriteBatch)` step is
one short transaction that opens and closes its handle (P8, §3.1). A crash mid-extraction
leaves the episode appended but un-extracted: the next `extract_uncached` pass picks it
up from the derived backlog. No retry queue is needed because the backlog is the set
difference.

The stage sequence is a **pure state machine** in core (P9):

```rust
pub enum Stage { Chunk, Extract, Validate, Resolve, Assert, Fold, Index, Done }

pub enum Step {
    Compute(Effect),        // model / embedding call — outside any transaction (§9.2)
    Commit(WriteBatch),     // one short transaction; opens + drops its handle
    Advance(Stage),
    Fail(ExtractionFailure),
}

pub fn step(stage: Stage, ctx: &JobContext, outcome: Outcome) -> Step;   // pure
```

The facade performs effects and applies batches. Crash-resume tests are table-driven over
`(Stage, Outcome) → Step`, with no database and no model — which is what makes §17.3's promised
crash tests writable.

### 9.2 The transaction rule

**No model call, no network call, and no embedding computation ever happens inside a database
transaction.** Stages compute outside, then commit a short batched write. A stalled provider —
or a slow local generation — must never block readers. Verified by a test that installs a
deliberately slow provider and asserts read latency stays inside budget.

Under C2 this rule matters more, not less: local generation is slower than a fast API call, and
there are more calls per ingest.

### 9.3 Chunking, with a context prefix we get for free

Chunk when content is both **long** and **entity-dense** — prose survives long contexts fine;
entity-dense input is what breaks extraction. Split recursively on a separator ladder ending in
the empty separator, which is language-independent by construction (a sentence-boundary
splitter would not be).

Each chunk is indexed with a **context prefix generated from the projection, not from a model**:

```
[2026-03-14 · Note: meeting.md · mentions: Alice(Person), ProjectX(Project) · community: infra]
<verbatim chunk text>
```

Every field is already known: mention spans, `occurred_at`, `SourceRef`, community membership.
Contextual retrieval reduces search failures substantially (published figures: −35% from
contextual embeddings alone, −49% with lexical fusion, −67% with reranking), and everyone else
pays a model call per chunk to produce that context.

**We do not.** This is the clearest case where owning a knowledge graph pays a retrieval
dividend that pure-RAG systems buy with tokens — and, being assembled from structured fields
rather than generated prose, it behaves identically in every language.

Model-generated context remains available as an opt-in upgrade, cached under an extractor id
like `extractions`.

### 9.4 Extraction contract

- **Output is schema-forced**, not prompted-and-hoped. Preference order **on the local path**:
  **grammar-constrained decoding** → forced tool call → schema-and-repair. On remote providers:
  provider structured-output → forced tool call → schema-and-repair. The port declares its
  capability; the mechanism is recorded in `ExtractorId`.
- **The grammar and the schema are both generated from the registry** (P4), so prompt/model
  drift is impossible by construction.
- **Validation rejects**: unknown predicates, subject/object type violations, cardinality
  violations, malformed or non-typed literals, spans that do not exist in the source, and **any
  entity mention not present verbatim in the episode**.
- **Repair loop**: one retry with validator errors appended, then partial acceptance — valid
  statements kept, invalid ones filed in `extraction_failures` with the raw response.

**Quote-based mention evidence (contract v2, ADR-006).** The model does not provide numeric
spans — measured on the default local extractor (2026-08-16), small models hallucinate byte
offsets even under grammar constraints, and the injection suite correctly forbids relocating a
wrong span to another occurrence. Instead each mention (and literal value) carries a `quote`:
a short snippet **copied verbatim** from the episode containing the surface. The server locates
the quote (first occurrence, exact bytes), requires the surface inside that window
(ASCII-case-insensitive fallback canonicalizes the surface to the source), and **derives the
byte span server-side**. Every gate survives: a fabricated surface has no copyable quote; a
quote that does not contain its surface is rejected outright; stored spans remain byte-exact
provenance; and instruction-shaped text that genuinely occurs in an episode stays data
(`injection_suite` variant B). Legacy span-format responses (cached extractions, older
providers) keep the span ladder: exact bytes → char-index → casing drift. `prompt_version`
bumped 1 → 2, so the two contracts never share an extraction cache.


**Why constrained decoding is primary now.** Owning the sampler makes schema validity a
token-level guarantee: the model cannot emit a token that would break the schema. That is
strictly stronger than provider structured-output, which validates after generation and
retries. Three consequences:

1. The repair loop is dead code on the local path — parse failures cannot occur.
2. `extraction_failures` narrows to **semantic** failures (wrong predicate, type violation,
   non-verbatim mention), which is exactly the set worth reviewing and learning from (§9.6).
3. **Small models become viable extractors.** Most of a small model's disadvantage on
   structured extraction is format compliance, not comprehension. Removing format failure by
   construction is what makes C2 affordable.

**What the verbatim-mention rule does and does not buy.** It structurally prevents the model
from *inventing an entity* — a fabricated name is not in the text, so the claim is dropped. It
does **not** prevent a false *relationship* between two entities that both genuinely appear.
The metric is therefore split (§17.2): fabricated-entity rate is a structural zero — **measured
by counting validator rejections, never asserted as a constant** — while relation precision is
an ordinary measured quantity defended by evaluation, not by architecture.

### 9.5 Extractor identity and upgrades

```
ExtractorId = blake3(model_id, model_digest, prompt_version, registry_major_version, mechanism)
```

`model_digest` (§8.4) is what makes a local weights swap a first-class, comparable event rather
than a silent quality change.

Upgrading is a normal operation: `oxibrain reextract --extractor <new>`. Old assertions remain,
new ones are added, both are comparable on the eval suite, and promoting — or demoting — the
new extractor is a config change.

### 9.6 Failure feedback

`extraction_failures` is browsable, re-runnable, and **read**: few-shot examples for the
extraction prompt are selected from the golden corpus and from repaired failures, keyed by
predicate. Changing examples changes `prompt_version`, hence `ExtractorId` — which §9.5 already
handles.

Under §9.4 this narrows usefully: with format failures impossible, everything in
`extraction_failures` is semantic and therefore worth learning from.

### 9.7 Cost and backpressure

- Extraction is **queued and rate-limited**, never synchronous with a user write unless
  `mode: sync` is requested.
- Budgets: max concurrent calls, max spend/day (tier 1+ only), max tokens/episode, and — new
  under C2 — **max local generation concurrency**, since local inference competes with the user's
  machine rather than with a rate limit.
- Profiles: `realtime`, `batched` (default), `nightly`.
- Three layers of idempotency, each a database constraint rather than a code path:
  the partial unique index on `(space_id, source_id, occurrence_id)` for event-path episodes,
  `PRIMARY KEY(episode_id, extractor_id)` on extractions, and content-derived `AssertionId`.
  Replaying the same source occurrence is therefore safe and cheap without conflating equal
  bytes from independent sources.

---

## 10. Identity and resolution

The hardest component, specified accordingly.

### 10.1 Pipeline

1. **Normalize** — NFKC, casefold, collapse whitespace, strip registry-declared affixes (§7.6).
2. **Block** — candidates from exact `entity_keys` hits plus **MinHash/LSH over character
   3-gram shingles**, with an **entropy gate**: short or low-entropy names skip the fuzzy path,
   because shingles over a two-character name are noise. Blocking is what keeps this sublinear
   as the graph grows, and under §7.3 it reuses shingles already computed.

   The entropy gate matters **more** under C3, not less: a two-character Chinese name is a
   complete, common, high-information name while a two-character Latin token rarely is. Entropy
   over shingles handles both without a script check — the gate is on information content, not
   on length.
3. **Score** — weighted: exact key, alias, **n-gram Jaccard** (§7.7), embedding similarity,
   **type agreement (hard gate)**, and **graph context overlap** (shared neighbours).
4. **Decide** on dual thresholds:
   - `≥ τ_high` → link to the existing entity.
   - `≤ τ_low` → create a new entity with this mention as its first key.
   - between → **create a new entity and record a merge candidate.** Never guess. Candidates
     surface in `oxibrain review` and the `review_merges` tool.
5. **Record the mention** verbatim with method and score, always.

```rust
pub struct ResolutionConfig {
    pub tau_high: f64,
    pub tau_low: f64,
    pub w_exact: f64,
    pub w_ngram: f64,                // replaces w_jw
    pub w_graph: f64,
    pub w_embedding: PerType<f64>,   // §10.3
}
```

### 10.2 Graph context is the positional evidence

With the prefix bonus gone (§7.7), **graph context overlap is the signal that separates two
people with the same name.** It costs one adjacency lookup, name collisions are the dominant
failure mode in a personal knowledge base, and it is evidence about the world rather than a
spelling heuristic.

It must be non-zero. v1.0 specified it and passed a constant `0.0` (F13).

### 10.3 Embeddings are a secondary signal for names

Embedding similarity over short proper nouns is weak. So:

- Name matching is **primarily lexical**. Embeddings are secondary — weighted low for `Person` /
  `Organization`, higher for `Concept`, where paraphrase is normal.
- Entity embeddings are computed over a **name + type + top-observations** context string, never
  a bare name.
- Thresholds are **per entity type and per embedding provider**, stored in config with defaults
  derived from the eval harness.

### 10.4 Merge and split

- `merge(a, b)` writes an `EntityMerge` and sets `merged_into`; lookups follow the redirect
  chain, path-compressed on read. Nothing is rewritten.
- `split(merge_id)` sets `undone_at` and re-runs resolution **for the affected mentions only**,
  using stored surface forms. Because P3 keeps every mention, this is exact rather than
  best-effort.
- **User merges are declarations** (§5.3), so reprojection replays them and never re-litigates
  them.

---

## 11. Retrieval

### 11.1 Three axes, not one enum

Retrieval separates **what you search for**, **how you search**, and **how you rerank**. A
single flat mode enum conflates them and cannot express "statements about X, lexical + vector,
reranked by graph distance from Y" — nor, under C3, the two lexical channels §7.4 requires.

### 11.2 The query

```rust
pub struct Retrieval {
    pub targets:  TargetSet,        // STATEMENT | ENTITY | EPISODE | CHUNK | COMMUNITY
    pub channels: Vec<Channel>,
    pub fusion:   Fusion,           // Rrf { k } | Weighted { .. }
    pub rerank:   Rerank,
    pub filters:  Filters,          // not optional, not ignorable
    pub limit:    usize,
    pub explain:  bool,
}

pub enum Channel {
    Lexical { index: LexIndex },    // Word | Ngram   (§7.4)
    Vector  { space: VecSpace },    // entity | statement | chunk
    GraphExpand     { seed: SeedPolicy, depth: u8 },
    CommunityExpand { seed: SeedPolicy },
}

pub enum Rerank {
    None,
    GraphDistance { from: Vec<EntityId> },
    Corroboration,
    Mmr { lambda: f32, max_similarity: Option<f32> },
    CrossEncoder,                   // via RerankPort
    Chain(Vec<Rerank>),
}

pub struct Filters {
    pub space:          SpaceId,
    pub as_of:          Option<Timestamp>,   // valid time
    pub known_at:       Option<Timestamp>,   // transaction time
    pub min_confidence: f32,
    pub trust:          TrustPolicy,
    pub predicates:     PredicateFilter,
    pub entity_types:   Option<Vec<EntityTypeRef>>,
}
```

Named **presets** cover the common cases and keep the old vocabulary: `Retrieval::hybrid()`,
`::lexical()`, `::semantic()`, `::graph()`, `::community()`.

### 11.3 Ranking is a pure decision (P9)

```rust
pub struct RetrievalInput {
    pub channels: Vec<ChannelResult>,               // one per executed channel, in spec order
    pub facts:    HashMap<TargetId, TargetFacts>,   // confidence, validity, trust, salience,
}                                                   // distinct_episodes, community, degree

pub fn rank(input: &RetrievalInput, spec: &Retrieval) -> RankingResult;
```

Three post-conditions, property-tested:

- **Conservation.** Every candidate appears in exactly one of `items` or `dropped`.
- **Filter totality.** No item in `items` violates `spec.filters`.
- **Determinism.** Equal inputs produce equal output, including tie-break order.

Conservation is what makes "instrument what you discard" structural: **you cannot silently drop
something from a function whose post-condition is that nothing is silently dropped.**

Store's job is mechanical: execute each `Channel`, batch one `TargetFacts` query, hand over
`RetrievalInput`. Filters that push down cheaply (space, entity type) go into SQL; filters that
require the fold (`as_of`, `known_at`, `min_confidence`) are applied by `rank`, so there is
exactly one place they can be forgotten and it has a test.

### 11.4 Rerankers

| Reranker | Cost | Why it earns its place |
|---|---|---|
| **Corroboration** | free | `Support { distinct_episodes, … }` is already computed and stored and currently affects nothing in ranking |
| **GraphDistance** | one adjacency lookup | Makes the graph pay as a ranking signal even if it under-delivers as a query structure (§23, D19) |
| **Mmr** | O(k²), k≈100 | Vector-only retrieval clusters similar memories and silently loses dissimilar-but-relevant context. MMR is the standard answer, and §16.3 has been citing the problem without it |
| **CrossEncoder** | a model call | Reranking is the single largest published step in contextual retrieval (−49% → −67%). Under C2 the model is local, so this is a latency choice, not a cost one |

### 11.5 Traversal, and why `as_of` is free

```rust
pub struct TraversalSpec {
    pub start: Vec<EntityId>,
    pub max_depth: u8,               // hard cap 5
    pub max_nodes: u32,              // hard cap, default 256
    pub predicates: PredicateFilter,
    pub direction: Direction,        // Out | In | Both
    pub filters: Filters,            // as_of, known_at, min_confidence, trust
    pub strategy: Strategy,          // Bfs | ShortestPath{to}
}
```

**The adjacency graph is belief-filtered.** Edges come from statements joined to beliefs, not
from statements alone:

```sql
SELECT s.subject_id, s.object_entity, s.predicate, s.id, b.confidence
FROM statements s JOIN beliefs b ON b.statement_id = s.id
WHERE s.space_id = ?1 AND s.object_entity IS NOT NULL
  AND b.status IN ('active','superseded')
  AND b.confidence >= ?2
  AND b.valid_from <= ?3 AND ?3 <= b.valid_to      -- as_of, sentinel-safe
```

Two things fall out, and they are the payoff for having built the fold:

- **Time-travelling traversal is free.** `traverse(…).as_of(t)` walks the graph as it was
  believed at `t`, because the edge set is a function of the fold and the fold takes a
  timestamp. No versioned edges, no snapshots.
- **Communities inherit the filter**, so a retracted relationship stops pulling two entities
  into one theme. Determinism holds provided clustering pins a stated `as_of` (the
  consolidation-window `now`) and records it with the community set.

Think-on-Graph is a *policy* over this primitive. Every traversal is bounded on depth, node
count and wall time — an unbounded walk driven by a model loop is a resource-exhaustion bug
waiting to happen.

### 11.6 Communities — the thematic layer

Entity-centric retrieval answers *"what do I know about Alice?"* It cannot answer *"what have I
been working on this year?"* — a question with no entity to anchor on. The fix: cluster the
entity graph, summarize each cluster, answer broad questions from summaries.

- **Label propagation, not Leiden.** Leiden re-clusters the world; label propagation updates
  incrementally, which is what a continuously-ingesting brain needs.
- **Votes are weighted by belief confidence**, not merely by edge multiplicity. A theme built
  from corroborated edges is worth more than one built from a single uncorroborated mention —
  and confidence is a signal we have and comparable systems do not.
- **Summaries are `Derived` episodes with cached text and computed uncertainty** (§5.3, §13.1).
  Searchable, quotable, provenance-carrying, and *terminal*.

Clustering is deterministic: fixed tie-break on entity id, fixed iteration cap, pinned `as_of`.
Only the summary *text* comes from a model, and that is cached. Recomputation runs in the
consolidation window, never on the write path.

### 11.7 Truth and salience are different things

PageRank/co-access importance, decay, and access frequency are **ranking signals only**. They
never affect whether something is believed. `Belief.confidence` and retrieval score are separate
fields computed by separate code, and both are returned so a caller can tell them apart.

This distinction is also the line P1 uses to split the projection (§5.1).

### 11.8 Explainability

Every result carries `provenance: Vec<EpisodeRef>` and an optional `explain` block: which
channel retrieved it, its rank in each list, the fused score, the rerank delta, the supporting
assertions, and the confidence breakdown. `oxibrain why <statement>` prints it; `oxibrain why
--dropped "<query>"` prints what was discarded and why, from `rank`'s conservation guarantee.

For a team deployment this is not a nicety; it is what makes the system auditable.

---

## 12. Context assembly

### 12.1 This function is the product

`assemble_context(query, token_budget)` is the single call an agent runtime makes per turn, and
the reason `oxios` can delete its memory code. It returns pinned facts, profile, high-salience
beliefs, the relevant neighbourhood, summaries-with-sources, and recent episodes, packed to
budget with provenance attached.

**This is reconstruction, not retrieval.** The context is composed on demand for *this* query
from beliefs, neighbourhoods and episodes; it is not a stored blob being fetched. That is the
distinction between graph memory and vector memory, and the reason it is a primitive rather
than a wrapper over `query`.

### 12.2 Layers, with the profile first

```rust
pub enum LayerKind {
    Profile,              // query-independent — what search cannot reach
    PinnedFacts,
    HighSalienceBeliefs,
    QueryNeighborhood,
    Summaries,            // never without sources (§12.4)
    RecentEpisodes,
}
```

**The profile is a standing query, not a new store.** Facts that must colour *every* answer are
never semantically near any particular query — a user who once said "call me by this name" will
never have that surfaced by a search for "plan a trip". Pure-search architectures have a
structural blind spot here.

```
profile(space) = render(
    beliefs where subject ∈ pinned_entities(space)
                and predicate.profile_relevant     -- registry flag, minor version
                and status = Active
                and confidence ≥ policy.floor
    partitioned by predicate.temporality)          -- Static → stable, Interval → recent
```

Two consequences: adding `profile_relevant` invalidates zero cached extractions (§5.5); and
**every profile line carries provenance, a validity interval, and a contradiction flag**, which
systems that maintain a profile document cannot offer. "Show me everything the brain thinks it
knows about me, with sources, and let me retract any line" falls out of the existing model.

### 12.3 Packing is a pure decision (P9)

```rust
pub struct ContextInput {
    pub profile:      Vec<ProfileFact>,
    pub beliefs:      Vec<RenderedBelief>,      // subject, canonical key, validity, support
    pub neighborhood: Vec<RenderedEdge>,
    pub episodes:     Vec<EpisodeExcerpt>,
    pub summaries:    Vec<SummaryWithUncertainty>,
}

pub struct PackPolicy {
    pub expand_top_k: usize,                       // episodes rendered in full
    pub expand_score: fn(&EpisodeExcerpt) -> f32,  // salience × confidence × recency
    pub belief_form:  BeliefForm,                  // OneLine | WithValidity | WithProvenance
    pub reserve:      Reserve,                     // floor share per layer
}

pub fn pack(input: &ContextInput, budget: &Budget, policy: &PackPolicy) -> ContextResult;
```

**Compress by default; expand only what the evidence justifies.** Most material ships as
one-line beliefs; only the top-k episodes ship verbatim. The expansion score is deterministic
and built from stored fields — no policy network.

Post-conditions: `total_tokens ≤ budget` **counted with `TokenizerPort`** (§7.5); the `Profile`
layer is never squeezed out by `reserve`; and §12.4's pairing rule holds.

### 12.4 Uncertainty is mandatory (P10)

`pack` **never emits a summary without its sources.** A post-condition, not a caller's
responsibility.

The evidence is direct: summary-only memory scored *below* the no-memory baseline in a
controlled comparison (2.65 vs 3.30) while summary-plus-sources scored highest (4.95). A design
in which summaries outrank their sources on salience is a design for the losing configuration.

---

## 13. Memory lifecycle

| Process | Does | Must not | Deterministic? |
|---|---|---|---|
| **Salience decay** | lowers retrieval weight of unused material | delete anything | yes |
| **Consolidation** (`dream`) | clusters related episodes, writes a `Derived` episode linked to its sources | overwrite or remove sources; be re-extracted | clustering yes, summary text cached |
| **Compaction** | moves cold episode content to a compressed column, keeping row, hash, links | break provenance | yes |
| **Retention** | per-space policy; expiry moves content to cold storage; removal requires redaction | run without an audit entry | yes |

Consolidation writing a derived episode instead of mutating memory is the key departure from
the inherited `dream`, which called `forget()` on merged-away entries. A summary is a new node
linked to its sources: the summary is queryable, the sources remain, provenance chains through,
and retrieval prefers the summary because salience says so — not because the sources are gone.

### 13.1 Uncertainty blocks

Every `Derived` episode carries:

```rust
pub struct Uncertainty {
    pub contradicted:       Vec<StatementId>,
    pub single_source:      Vec<StatementId>,        // support.distinct_episodes == 1
    pub stale:              Vec<(StatementId, Timestamp)>,
    pub excluded_untrusted: u32,
}
```

**Computed from the fold, never written by the model.** So it cannot go stale when a cached
summary does, and it is not something a model can be persuaded to omit. Every input already
exists — `Support`, `BeliefStatus::Contradicted`, `valid_to`, trust weights — and v1.0 discarded
all of it at the summary boundary.

Compaction stores compressed content **in SQLite**, not in an external blob store.

### 13.2 Consolidation vs. application note curation

`Brain::consolidate` is **not** an authoring path. It is a read/derive operation on the
ledger: it clusters ledger episodes by topic or temporal proximity, emits a new
`EpisodeKind::Derived` episode (or cache-links an existing one) whose content is the cached
extraction summary, and writes that derived episode into the ledger as an ordinary immutable
row. Application note curation — a user editing a memo, oximemo's user reordering a card, an
Oxios agent rewriting its scratchpad — is editing the host's own files and never crosses the
memory plane boundary. The brain does not maintain a parallel curation surface and never mutates
an `EpisodeKind::Derived` episode in place. Re-deriving a cluster (because the underlying
model changed, or because new source episodes arrived) writes a **new** derived episode; the
previous one stays, and `Belief` / `support` / `Uncertainty` continue to be computed from the
source ledger, never from a derived episode (`§5.1`, P1; `§13.1`).

Concretely:

- **Extracts from primary episodes.** Consolidation clusters ledger episodes and re-reads
  them through the extraction cache; it does not re-extract a previously derived summary
  and does not let one derived episode feed another as a primary source.
- **Preserves support.** The `Derived` row records its source episode ids and the
  `Support` slice (distinct sources, contradictions, single-source claims) that the fold
  produced for it. P10 is enforced at write: a derived episode with empty support is
  rejected.
- **Preserves `Uncertainty`.** The summary's `Uncertainty` block is the same block the
  fold computes for any episode, recomputed on read, never whittled down at the summary
  boundary. A summary that hides its contradictions is a regression, not a feature.

---

## 14. The views surface

### 14.1 Agents navigate; they are not only handed context

Retrieval quality is not the only axis. Published ablations on wiki-structured retrieval put
**iterative link-following at roughly twice the contribution of the structure itself**, and
memory-as-a-filesystem products reach for the same affordance so agents can explore rather than
receive.

Two tools. Not more — a large tool surface is a warning, not a target. **Fifteen MCP tools is
the cap** (§15.2).

```
brief(target: Entity | Topic | Space, depth) -> Markdown
navigate(from: ViewId, link: LinkId)         -> Markdown
```

`brief(entity)` renders, from data that already exists: identity and aliases; current beliefs
with validity, confidence and support; **contradictions with both provenances**; **uncertainty**
(§13.1); neighbours as followable links; timeline change points; sources.

CLI parity: `oxibrain page <entity>`.

### 14.2 Views are rendered, never stored

A stored page can disagree with the fold, and then we own an invalidation problem the whole
architecture exists to avoid. Systems that materialize a wiki need a `lint` pass and an error
log to repair drift; a rendered view has no debt because it has no lifetime.

> **A materialized wiki is repaired with `lint`. oxibrain regenerates with `reproject`.**

Testable: `brief(e)` twice on an unchanged ledger must be equal, modulo cached summary text.

`oxibrain-views` is a separate crate and **must not name `rusqlite`** — a crate boundary is the
only way to make "views do not reach the database" checkable rather than aspirational.

---

## 15. Security, tenancy, trust

### 15.1 Spaces

Every episode, entity, statement, and query is scoped. Spaces are hard boundaries: no query,
traversal, or **write** crosses one. Enumeration and resources obey the same boundary: a scoped
session lists and reads only the spaces in its scope; resource reads are gated exactly like
tool calls.

**A space is a privacy boundary, never an application boundary.** Several apps writing to one
space is the entire point — the brain can only connect last week's note to a Tuesday routine to
yesterday's agent session if they land together. Apps are distinguished by `SourceRef`, a
*label*, not a boundary.

**Cross-space reference is post-v1.** The v2 rule, stated now so the schema does not preclude
it: resolution may consult `shared` as an additional **read-only candidate source**; writes never
cross; a local entity linked to a shared one records that link explicitly.

**Lifecycle (v2.12).** Spaces are created by `init`/`space add` (with per-space
vault provisioning under `~/.oxi/vault/<space>/` for the default brain dir) and
removed by `space remove` (empty-only, scaffold-aware) or `space remove --purge`
(audited `RedactTarget::Space`; vault files are never deleted — §1.4). The
default space lives in `~/.oxi/config.toml` and resolves identically for CLI
and MCP. Provisioned vault roots persist in `documents.toml` as expanded
absolute paths — loading expands a leading `~` against `$HOME`, and
provisioning writes the resolved path, never the tilde literal.

### 15.2 Capabilities

```rust
pub struct Scope {
    pub spaces: Vec<SpaceId>,
    pub caps: CapabilitySet,        // Read | Write | Ingest | Sample | Admin | Redact | TrustedIngest
    pub predicate_filter: Option<PredicateFilter>,
    pub entity_type_filter: Option<Vec<EntityTypeRef>>,
    pub expires_at: Option<Timestamp>,
    pub label: String,              // authenticated principal label recorded at ingress
}
```

Tokens: `oxibrain token issue --space work --caps read,query --expires 30d`. MCP clients present
one. The `serve --http` console and stdio children are loopback-only by default; non-loopback
binds require TLS and refuse to start without it (§15.6).

`Sample` is a distinct capability (§8.3). `TrustedIngest` is the narrowly scoped authority to
override server trust evaluation and explicitly mark an ingest as trusted (§15.3).

### 15.3 Trust tiers and prompt injection

`ingest` runs a model over arbitrary text, and that text can contain instructions. Trust is
**server-evaluated**: clients may submit source claims, but they cannot assign an authoritative
`TrustTier`. Effective trust comes from the applicable source-policy declaration for the
registered source. Source registration and policy changes are themselves append-only
`Declaration` episodes, so replay can reproduce the assessment instead of consulting mutable
external configuration.

`TrustedIngest` is the sole exception. A scope carrying that capability may explicitly request
`Trusted`; without it, an ingest request that attempts to self-grant `Trusted` is rejected and
the server applies source policy. Ordinary `Ingest` or `Write` authority never implies
`TrustedIngest`.

| Tier | Source policy posture | Treatment |
|---|---|---|
| `Trusted` | registered human-authored sources, direct declarations | full weight |
| `SemiTrusted` | conversations, agent traces, reviewed artifacts | full weight, flagged |
| `Untrusted` | web clips, imported third-party material, unreviewed scratch content | content fenced and marked as data; assertions get reduced trust weight and are **excluded from context assembly by default** unless corroborated by a trusted episode |

Additionally: extraction output is data, never executed; validated mentions must appear
verbatim (§9.4); assertions retain the server-evaluated trust of their supporting episode; and
assertions from a single untrusted episode can never alone flip a belief with trusted support.

**C2 changes the threat surface, and improves it.** With a local extractor, untrusted content
is processed by a model we ship, under a grammar we generate, on the user's machine — not sent
to a third party. Injection remains a real risk to *content*; it is no longer also an
exfiltration risk by default.

### 15.4 Audit

Append-only `audit_log` of every write, redaction, merge, token issue, scope grant, sampling
authorization, model swap, and config change: actor, scope, operation, target, timestamp. Not
rebuildable, so it backs up with the ledger.

### 15.5 Redaction — the only true delete

`redact(target, reason)`, where target is an episode, an entity, or a predicate-scoped subset:

1. Resolve the closure: episodes, extractions, summaries, chunks, mentions, assertions, and
   statements left unsupported.
2. Write the audit entry with the reason — **before** acting.
3. Overwrite `content`, `raw_response`, and summary `text` with a tombstone; keep row, id,
   hashes, timestamps.
4. Delete affected mentions and assertions; re-fold beliefs; delete statements with zero
   remaining support.
5. Rebuild affected ranking-half state and communities; verify no orphans
   (`oxibrain doctor --check-orphans`).

`redact --dry-run` prints the closure first. Redaction is idempotent and reports exactly what it
removed. "Forget this person entirely" is a supported, tested operation.

### 15.6 At rest, in transit, across devices

- Optional whole-database encryption (SQLCipher) behind a feature flag, key from the OS
  keychain. Off by default because it complicates backup; documented.
- HTTP is loopback-only by default; a non-loopback bind requires TLS and refuses to start
  without it.
- **Sync is post-v1**, and the schema is ready: deterministically derived ids, content hashes,
  event identity, an append-only ledger, and a rebuildable projection. The mechanism will be
  **ledger log shipping** (append-only and commutative) plus **Loro** (Rust CRDT) for the
  mutable slices needing real merge semantics: user merges, resolutions, config. Derived state
  is never synced; each device reprojects.

### 15.7 Foundation profile boundary — secrets, scopes, and session transport

The Foundation v1 contract (`doc/spec/oxi-foundation-v1.md`, ADR-007) governs how a host
process resolves a profile's Keychain-locator reference into a usable credential. The
rules below are enforced at the **facade/CLI boundary** of `oxibrain` (and of every
consumer of `oxibrain-client`). `oxibrain-core`, `oxibrain-store`, and `oxibrain-index`
never see a Keychain reference, a profile JSON path, or a provider secret. They take an
`LlmPort` (or `EmbeddingPort`) that is already wired; they do not know which profile
produced it.

- **Profiles carry a locator, not a secret.** `profiles.json` is non-secret by construction:
  a profile's `credential` field is `{service, account}` — the OS-Keychain service and
  account names under which the secret is stored. The profile is world-readable; the
  secret is not. Storing `api_key`, `bearer`, `access_token`, or `refresh_token`-shaped
  fields in `profiles.json` is a parser-level rejection. Environment variables remain an
  explicit development/automation override, never the Foundation path.
- **Role binding is enforced before the secret is read.** A profile declares the roles it is
  permitted to satisfy (`memory.extract`, `memory.consolidate`, `coding.primary`,
  `assistant.general`). The host asks for a role, not for a provider secret. Resolution is:
  explicit CLI/environment override → a Foundation profile whose `roles` contains the
  requested role → existing `ANTHROPIC_*`/`OPENAI_*` compatibility environment
  variables → local model. A profile whose role list does not contain the requested role
  is **not selected**, and a missing or unavailable Keychain secret is reported as such
  rather than silently falling through to a different remote provider.
- **Session transport is explicit, never auto-discovered.** v2.11 deletes the
  `default_socket_path` / `connect_default` / `connect_endpoint` surface and the
  `~/.oxi/brain/oxibrain.sock` listener. The canonical transport is a caller-owned
  child: `oxibrain-client` builds a `LocalProcessEndpoint { executable, dir }` and
  spawns `executable serve --stdio --dir <dir>`; the child retains protocol state and
  optional loaded models but opens databases per request, and dies when stdin closes
  (or when the `BrainClient` is dropped). There is no ambient endpoint: a test, a CI
  runner, and a host process all pass an explicit `--dir`, never discover a default.
  External MCP hosts use the same stdio command.
- **Auth-first-message and scope semantics are preserved.** The MCP tool surface stays
  at fifteen. The `handshake` native JSON-RPC method still rides stdio as the
  capability-negotiation channel (used by `oxibrain-mcp` for its v1.0 `ClientHello` /
  `ServerInfo` exchange with the daemon-free `serve` child) and the existing `Scope` /
  `Capability` model from §15.1–§15.2 is unchanged. Capability metadata never replaces
  a token and never broadens scope. Full enumeration of the additive client surface is
  in `doc/CONSUMPTION_CONTRACT.md`.

---

## 16. Interfaces and operations

### 16.1 Rust API

```rust
let brain = Brain::open(BrainConfig::at("~/.oxi/brain")).await?;   // embedded, handle-free
let ep   = brain.ingest(Episode::note("meeting.md", text)).await?;
let ctx  = brain.assemble_context("what did we decide about auth?", 3_000).await?;
let resp = brain.search(Query::hybrid("auth decision").planes(both!())).await?;   // SearchResponse { memory, documents, freshness }
let sub  = brain.traverse(TraversalSpec::from(entity).depth(2).as_of(date)).await?;
let pg   = brain.brief(Target::Entity(id)).await?;
brain.declare(Statement::new(alice, "works_on", projectx)).valid_from(date).await?;
let fresh = brain.index_documents(IndexOptions { embed: true, budget: None }).await?;
let hist  = brain.document_history("personal", "vault", "notes/auth.md", 32).await?;
```

One typed surface: the **embedded `Brain` facade** is the canonical API — `Clone`,
cheap (config + ports only, no store actor, no writer thread, no reader pool; §3.1,
P8). Operations open their store for the duration of one method and drop the handle.
`oxibrain-client::BrainClient` is the **remote surface** for stdio/HTTP sessions —
the same methods, transported as JSON-RPC. Unification is post-v1, triggered by a
consumer needing topology switching; LLM-injecting methods cannot cross a process
boundary by construction.

### 16.2 MCP surface

| Tool | Caps | Notes |
|---|---|---|
| `search | Read | channels/fusion/rerank presets; `as_of`, `known_at`, `min_confidence`; v2.11 gains optional `planes` (`Memory` / `Documents` / both, default both) and returns `{memory, documents, freshness}` |
| `recall | Read | context assembly — the per-turn call for agents; gains a `Documents` layer between query neighborhood and recent episodes |
| `brief | Read | rendered entity/topic page with followable links |
| `navigate | Read | follow a link from a page |
| `get_entity | Read | entity + current beliefs + aliases + neighbours |
| `traverse | Read | bounded subgraph; belief-filtered; `as_of` supported |
| `timeline | Read | belief intervals over a range |
| `why | Read | provenance, confidence breakdown, and drops |
| `contradictions` / `review_merges` | Read | inboxes |
| `stats | Read | counts (v2.11 gains pending extraction count/oldest seq + per-root document counts) |
| `ingest | Ingest | long-running task |
| `remember | Write | one-shot ingest + inline extraction; returns `Captured` or `CapturedPending` |
| `declare` / `retract | Write | deterministic writes, no model |
| `merge_entities | Write | resolution maintenance |
| `redact | Redact | destructive; separate capability on purpose |

**Fifteen. That is the cap.** Adding a sixteenth requires removing one.

**Default-space resolution (v2.12).** A tool call that omits `space` resolves
the configured default — `~/.oxi/config.toml`'s `default_space` (§18), falling
back to `"personal"` when no config exists — the same `--space` > config >
builtin chain as the CLI (§16.4). `serve --stdio`/`serve --http` load the
config once at startup and hand the value to the session. No tool or verb
implicitly creates a space: unknown names fail fast with a `space add` hint
(creation is `init`, `space add`, or archive import only).

Resources: `spaces://` (list), `space://`, `entity://{id}`, `episode://{id}`, `graph://{entity}?depth=n`, `doc://<alias>/<locator>?rev=<rev>` (v2.11).

**Schema evolution is additive, as §19.4 requires.** The v1.0 `search` and `traverse` schemas
never exposed `as_of` or `min_confidence` (F29), so adding them is purely additive — no client
breaks, and the new expressiveness is opt-in. v2.11's `search.planes` parameter is additive
(default both; an old client sees only the unchanged `memory` half). `mode` remains a string
enum whose values are now preset names. The one **corrective** change: `recall`'s advertised
description promised layers that were never populated (F30); the description must match what
is returned.

Native JSON-RPC methods — `handshake`, `reproject`, `spaces/list`,
`document_history` (v2.11) — are the first-party surface; they are not MCP tools and do
not count against the cap. `document_history` is read-gated like `resources/read`. The
v2.10 `sync/run` method is removed (no daemon, no watcher); `episodes/for_locator` is
removed (semantic occurrence history is replaced by gix-backed document history).
### 16.3 Concurrency, budgets, observability

- **One writer per store, scoped to one operation** (P8, §3.1). Each store has its
  own advisory lock (`brain.lock`, `documents.lock`); a read method opens a WAL
  reader per call and drops it; a write method acquires the lock, opens a writer,
  commits a short batch, and drops the handle. Bounded retry `[25, 50, 100, 200,
  400, 800]` ms, then `BrainError::Locked` with the holder path. Models may live in
  the process; databases may not.
- **Performance budgets** at 10⁵ episodes / 10⁵ entities / 10⁶ assertions on a laptop:

| Operation | p95 budget | Last measurement (200 ent / 500 stmt fixture, Apple M4) |
|---|---|---|
| declaration write | < 5 ms | **0.38 ms** ✅ |
| `get_entity` | < 10 ms | **0.16 ms** ✅ |
| hybrid query (top 20) | < 80 ms | **1.44 ms** ✅ |
| traversal, depth 3, ≤256 nodes | < 100 ms | **0.29 ms** ✅ |
| `assemble_context` (3K tokens) | < 150 ms | **0.19 ms** ✅ |
| reproject from cache (whole store) | < 5 min | **42.7 ms** ✅ |
| cold start (Brain::open) | < 2 s | not yet benchmarked — operation-scoped handles change the curve |
| `brief` (entity, depth 1) | < 100 ms | new in M9 |
| local extraction (one episode) | reported, not budgeted | **~13 s** (Qwen2.5-1.5B-Instruct Q4_K_M, Apple M4 Metal, 512 out tokens, 2026-08-13 spike) |

- **Instrument what was discarded, not only what was returned.** Recall logs what it dropped and
  why — below confidence floor, outside the valid-time window, trust-excluded, truncated by
  budget — and `oxibrain why --dropped` prints it. This is guaranteed by `rank`'s conservation
  post-condition (§11.3), not by discipline.

### 16.4 CLI

```
oxibrain init [--space] [--dir D]              # provisions ~/.oxi/vault/<space>/ + its root when no --dir and $HOME resolves (§15.1)
oxibrain spaces                                # list spaces with episode + document counts; * marks the config.toml default (v2.12)
oxibrain space add <name>                      # create (idempotent) + provision ~/.oxi/vault/<name>/ + its documents.toml root (default dir only; §15.1)
oxibrain space remove <name> [--purge]         # empty-only removal; --purge = audited RedactTarget::Space + documents cache sweep; vault files never deleted
oxibrain space default [<name>]                # print or set ~/.oxi/config.toml default_space (§18)
oxibrain doctor [--dir D]                      # freshness, skipped roots/files, dangling doc:// refs, legacy pull sources, pending stats, model digest
oxibrain stats [--dir D]                       # counts (memory + per-root documents + pending extraction)
oxibrain ingest <path|-> [--source kind] [--space s]   # trust is server-evaluated
oxibrain remember "<text>" [--space s]         # one-shot ingest + inline extraction; returns Captured or CapturedPending
oxibrain index [--documents] [--embed] [--dir D]        # §4.2.1 — reconciles + (optionally) embeds
oxibrain extract --pending [--limit N] [--dir D]        # queue-less; walks uncached_memory_episodes
oxibrain ask "<question>" [--as-of DATE] [--global] [--explain]
oxibrain page <entity>
oxibrain entity show|merge|split|alias|retract          # §4.2.2 — merge/split are inverse (D34); alias +retract are Declarations
oxibrain declare <subject> <predicate> <object>
oxibrain source policy <source-name> --trust <tier>     # §4.2.2 — server-evaluated trust via Declaration
oxibrain document-history <alias> <locator> [--space s] [--limit N]   # §4.2.1 — gix read history
oxibrain timeline <entity> [--from --to]
oxibrain why <statement-id> | why --dropped "<query>"
oxibrain contradictions | review
oxibrain model list|pull|verify|use           # C2
oxibrain reextract [--extractor X] [--since] | reproject | regenerate-summaries
oxibrain redact <target> [--dry-run] --reason "..."
oxibrain export [--format jsonl|md] | import
oxibrain serve --stdio [--dir D]                # caller-owned child; dies with stdin
oxibrain serve --http <addr> [--dir D]          # foreground loopback console; no `--daemon`, no `--socket`
oxibrain token issue|list|revoke
oxibrain predicate add|list | eval [--suite fast|full|bench|parity]
# removed: sync, sync/run, watcher flags, --daemon, --socket, default-socket discovery
```

Every `--space`-carrying verb resolves its target as `--space` >
`~/.oxi/config.toml` (`default_space`, §18) > `"personal"` — the same chain on
CLI and MCP (§16.2). Unknown spaces fail fast with a `space add` hint; nothing
implicitly creates a space — only `init`, `space add`, and archive imports do.

`index [--documents] [--embed]` is the document-plane maintenance verb. It loads
`documents.toml`, opens `documents.db` for a short locked apply, runs
`core::documents::diff_roots` against the cached manifest, scans each root
(`oxibrain-connectors::scan_root` for plain roots, `GitDocumentReader::open` for
git roots — `Ok(None)` when not a repo), and applies the resulting `ApplyPlan` in
one transaction. `--embed` walks `pending_vector_chunks(...)` and calls the
configured `EmbeddingPort`. `index --documents` is also what `search` / `recall`
invoke when a configured root has not yet been materialized, so the cache converges
with the filesystem by the time the first document hit lands.

`serve --stdio --dir <dir>` is the canonical session transport: a child of an MCP or
library session that exits when stdin closes. The same binary serves `--http` for
the operations UI, loopback-only by default. There is **no daemon**, **no
`--socket`**, and **no discovery** — every caller passes an explicit `--dir`.

`serve --http <addr>` also serves the **embedded console** (§16.6) by default. The `--ui-dir`
flag is retained only as a dev override that points at a built bundle on disk (see §16.6);
production callers omit it and use the embedded assets.

### 16.5 Import / export, backup, errors

- Full-fidelity JSONL export of ledger + cache + audit, round-trip tested: `export | import`
  into an empty store, then `reproject`, yields a byte-identical **truth half**.
- `oxibrain backup` uses SQLite's online backup API (WAL-safe), with `--no-projection` (always
  safe) and `--no-cache` (smaller; restore needs re-extraction and a model re-fetch).
- `BrainError` variants each document whether they are retryable and whose fault they are:
  `Config, Storage, Migration{found,expected}, Locked{holder}, Scope{required}, NotFound,
  `Config, Storage, Migration{found,expected}, Locked{holder}, Busy (documents.db
  cache lock, distinct from `Locked{holder}`), Scope{required}, NotFound,
  Invalid(ValidationReport), Extraction, Provider{retryable}, Budget, Conflict, Corruption,
  Model{missing_or_corrupt}`. Ports return typed errors; `anyhow` never crosses a public
  boundary.

The `oxibrain` binary serves a small, opinionated **console** for inspecting and operating
a brain instance. The bundle lives in `crates/oxibrain-mcp/assets/dist/` (vite's `outDir`,
inside the owning crate so `cargo package` ships it) and is **compiled into the
binary** via `include_dir!` (the `include_dir` crate, ADR-008). No separate Node tool
chain, no `--ui-dir` flag, no per-platform installer — `cargo install oxibrain-cli && oxibrain
serve --http 127.0.0.1:18080` opens a working console at the served root. `--ui-dir` is
retained as a development-time override pointing at a freshly built bundle on disk
(`bun run build` in `apps/brain-ui/`); production callers never need it.

**Scope.** The console covers the repair/operations slice of the ecosystem blueprint
(blueprint §6.4) — knowledge quality work and operations, **not** knowledge creation or
exploration. The seven routes that ship:

| Route | Purpose |
|---|---|
| Overview | store health and counts (entity/episode/assertion/statement totals; last reproject time) |
| Entity | rendered entity page (`oxibrain-views::brief`); entity detail with aliases, neighbours, and provenance |
| Conflicts | contradiction inbox surfaced from the fold; supports review/retract |
| Merges | merge review queue, with apply/undo backed by `Declaration` episodes |
| Failures | quarantine inspection — failed extractions with raw response and error trace |
| Sources | source registry + effective source-policy trust per source |
| Operations | `reproject`, `doctor`, store-size readout, and model-digest verification |

Each route reads through the same MCP/JSON-RPC surface (§16.2). The Merge/Failures/Sources
tables all dispatch into the `review_merges` tool's `section ∈ {merges, failures, sources}`
switch, which keeps the MCP tool surface at the fifteen-tool cap. The Operations route's
`reproject` is served as a bare JSON-RPC method (not an MCP tool — too destructive for agent
access); it returns before/after space stats so the console confirms what changed.

**Out of scope.** The console deliberately does **not** include ask/chat, capture/authoring,
a general exploratory force graph, or note/task/session management. Those are the user-facing
left hand of the oxi ecosystem and live in dedicated apps (`oximemo`, `oxiline`, `oxios`,
third-party MCP clients). oxibrain stays on the right hand of "understand, then operate";
shipping them here would turn a memory kernel into a general productivity app and break the
editing/ownership boundary in §1.4.

**Bundle delivery.** `crates/oxibrain-mcp/assets/dist/` is **committed to the repo** (it is
the only guarantee that `cargo install oxibrain-cli` from a tagged release produces a
runnable console, and it must sit inside the crate for `cargo package` to embed it in the
tarball).
A CI job builds the bundle with `bun install && bun run build` in `apps/brain-ui/`, then
asserts `git diff --exit-code crates/oxibrain-mcp/assets/dist` and an aggregate gzip size
≤ 400 KB, blocking any release whose bundle diverges or grows beyond budget.
Schema/asset mismatches therefore fail the build, not the user.

---

## 17. Quality and evaluation


Without measurement, "the extraction pipeline is the product's value" is unfalsifiable and every
tuning decision is a guess.

### 17.1 Corpora

**Our golden corpus** — ~200 labelled episodes across note, document and agent-trace shapes with
annotated entities, statements and validity intervals, plus ~100 questions with reference answers
and required supporting episodes. The controlled comparison (§17.2) runs on this corpus; the
categories that matter are knowledge update and temporal reasoning.

**The parity corpus, for C3** (§7.8) — seven writing-system property classes, ~20 episodes each.
This replaces v1.0's bilingual KO/EN scope, which was the right instinct for a Korean author and
the wrong scope for an international product.

### 17.2 The controlled comparison

Absolute benchmark scores overstate memory-system gains when non-memory factors are not
controlled; simple baselines frequently match complex ones. So the headline number is a
**delta**, measured on our own corpus:

| Arm | Configuration | Role |
|---|---|---|
| **(a)** | full context, no retrieval | ceiling |
| **(b)** | lexical + dense chunks + RRF, **no knowledge graph** | **the control** |
| **(c)** | oxibrain complete | treatment |

**Report (c) − (b), per category, with tokens/query alongside**, and run every arm under both
extractors (local tier 0 and frontier tier 1). Publishing only the frontier number would hide
whether C2 is viable, which is the thing we most need to know.

The categories that matter are the ones the assertion log and the bi-temporal fold exist for:
knowledge update and temporal reasoning.

### 17.3 Metrics and gates

| Metric | Target | CI gate |
|---|---|---|
| **Fabricated-entity rate** | 0.00 | hard zero, **measured from validator rejections** |
| Statement precision (relations) | ≥ 0.90 | block on > 2pp regression |
| Statement recall | ≥ 0.70 | block on > 3pp regression |
| Entity resolution F1 | ≥ 0.92 | block on > 2pp regression |
| Wrong-merge rate | ≤ 0.01 | hard cap |
| Retrieval recall@10 | ≥ 0.85 | block on > 3pp regression |
| Temporal QA accuracy | ≥ 0.80 | block on > 3pp regression |
| Answer-with-correct-provenance | ≥ 0.95 | block on any regression |
| **Cross-property variance** (§7.8) | ≤ 10pp | hard gate |
| **(c) − (b) on temporal categories** | > 0, materially | reported every run; drives §20 |

Absolute targets are provisional until the first full run; **the regression gates are the real
contract.**

Two suites: `fast` (fixture-replayed responses, no network, every PR) and `full` (live
extraction, nightly and on extractor changes), plus `parity` (the C3 gate).

### 17.4 Testing strategy

- **Truth reprojection determinism** — for a randomly generated ledger, `reproject()` produces a
  truth half **byte-identical** to the incrementally built one. The single most valuable test in
  the suite; may never be disabled.
- **Ranking equivalence** — same membership, recall@10 within the §5.1 tolerance, across two
  backends.
- **Property tests** on the temporal fold, interval algebra, canonical serialization, RRF, MMR,
  n-gram similarity, `rank` (conservation, filter totality, determinism), and `pack` (budget
  soundness, summary-source pairing).
- **Migration chain tests** from every historical schema version.
- **Crash tests** — table-driven over the `step` state machine, plus end-to-end kills mid-ingest.
- **Fuzz** — extraction response parser against malformed and adversarial input.
- **Injection suite** — instruction-shaped episode text; assert nothing escapes the validator
  and trust weighting holds.
- **Degradation test** — the brain unreachable; assert every consumer-facing API fails fast with
  a typed error rather than hanging (the ecosystem's C1 contract).
- **Parity suite** — §7.8's variance gate.

---

## 18. Workspace layout

```
oxibrain/
├── AGENTS.md
├── doc/
│   ├── ARCHITECTURE.md        # this file — canonical
│   ├── ROADMAP.md             # sequencing, exit criteria, effort
│   ├── ECOSYSTEM.md           # cross-project architecture
│   ├── CONSUMPTION_CONTRACT.md
│   ├── spec/                  # per-milestone implementation specs
│   ├── adr/                   # architecture decision records
│   ├── research/              # dated reviews; historical, not authoritative
│   └── ontology.md            # generated from the core registry
├── tests/
│   └── fixtures/oxi-foundation/v1/   # cross-host fixture corpus (ADR-007)
├── crates/
│   ├── oxibrain/              # facade: `Brain`, config, prelude — target < 1,000 LOC
│   ├── oxibrain-core/         # pure: fold · resolve · rank · pack · step · registry
│   ├── oxibrain-index/        # pure algorithms: ngram · blocking · rrf · mmr · knn · quantize
│   ├── oxibrain-store/        # SQLite: schema, migrations, writer actor, fetchers, appliers
│   ├── oxibrain-views/        # brief · navigate · profile — must not name rusqlite
│   ├── oxibrain-ports/        # Llm · Embedding · Tokenizer · Rerank · Clock (+ fakes)
│   ├── oxibrain-llm-local/    # GGUF inference + GBNF + TokenizerPort      [default]
│   ├── oxibrain-embed-local/  # multilingual encoder                       [default]
│   ├── oxibrain-llm-http/     # anthropic/openai/ollama adapters           [feature]
│   ├── oxibrain-embed-http/   # hosted encoders                            [feature]
│   ├── oxibrain-connectors/   # markdown vault, directory, chat, stdin
│   ├── oxibrain-mcp/          # MCP server adapter + sampling LlmPort
│   ├── oxibrain-client/       # thin client for consuming apps (spawn_local)
│   └── oxibrain-cli/          # THE binary: `oxibrain` (cli + serve --stdio + serve --http)
├── eval/                      # golden corpus, parity corpus, benchmark runners
└── apps/                      # brain-ui source — its `dist/` is embedded into the binary (§16.6)
```

**Installation root.** Every consuming app reads from a single tree, which `oxibrain init`
creates and any caller can open via `--dir <dir>` thereafter. Clients never read SQLite
directly; they reach the brain through a caller-owned stdio child (or in-process `Brain`).

```
~/.oxi/
├── config.toml                 # default_space (v2.12; dir/provider reserved) — strict parse
├── foundation/v1/              # Foundation v1 contract (ADR-007) — non-secret
│   ├── profiles.json           # provider profiles (Keychain locator only — never a secret)
│   └── packages.lock           # resolved Foundation packages (name/version/digest/source/trust)
├── vault/                      # per-space note vaults (v2.12) — provisioned by init/space add; §15.1
│   └── <space>/                # one directory per space; lands in documents.toml as an expanded absolute root
└── brain/                      # oxibrain data directory (`<dir>``, default ~/.oxi/brain)
    ├── brain.db                # memory ledger + projections (schema v11)
    ├── brain.lock              # advisory lock on brain.db
    ├── documents.db            # disposable document cache (schema v1)
    ├── documents.lock          # advisory lock on documents.db
    └── documents.toml          # configured document roots (§4.2.1)
```

**Dependency rules, enforced in CI:**

1. `oxibrain-core` may depend on `ports` and `index`. Never `store`, never an adapter, never
   `oxicode-*` or `oxios-*`.
2. `oxibrain-core` and `oxibrain-index` must not name `rusqlite`, `tokio`, or `reqwest`.
3. `oxibrain-store` must not name `rank`, `pack`, or `step`. It may name their input and output
   types. **This is the enforceable form of P9.**
4. `oxibrain-views` must not name `rusqlite`.
5. Only `oxibrain-store` may reference `rusqlite`.
6. **No crate outside `oxibrain-index` may contain a natural-language word list, stemmer, or
   script check.** **This is the enforceable form of P11.** Registry-sourced affix lists are
   data and exempt.
7. Surfaces depend on the `oxibrain` facade only.
8. Default features pull **zero** oxi-ecosystem crates.

---

## 19. Relationship to the oxi ecosystem

### 19.1 One brain, several apps

oxibrain is **infrastructure for the ecosystem and a product for the individual** — both,
deliberately.

```
oximemo        oxiline        oxios        Claude Desktop
(capture)      (time)         (agents)     (external)
   └──────────────┴─────────────┴──────────────┘
       stdio child / in-process Brain facade (no daemon)

**Contract: the brain is additive, never load-bearing.** With the brain offline (no
caller-owned child is currently running), every consuming app retains its primary
function. Each app keeps owning its own source of truth. **oxibrain understands; it
does not own.**

Where this is weakest, stated honestly: `oxios`. After migration it has no memory code of its
own, so a brain outage leaves its agents with *no* memory rather than degraded memory. See
`doc/adr/ADR-002` for the fallback decision.

### 19.2 Consumption contract

Detail in `doc/CONSUMPTION_CONTRACT.md`. v2.11 retires the Foundation handshake helpers
(`default_socket_path`, `connect_default`, `connect_endpoint`) and replaces them with
`LocalProcessEndpoint { executable, dir }` plus caller-owned `spawn_local` /
`spawn_local_with_token`. The MCP tool surface is unchanged; the `handshake` native
JSON-RPC method still rides stdio for capability negotiation between the daemon-free
`serve` child and its caller.

- Semver on the `oxibrain` facade. The public surface is `oxibrain::*`.
- MCP tool schemas versioned; **additive changes only within a major** (§16.2).
  The fifteen-tool MCP surface is unchanged; the v2.11 `search.planes` and
  `StatsCounts` additions are additive-only. The `document_history` native JSON-RPC
  method joins `handshake`, `reproject`, and `spaces/list` and does not count.
- Stability tiers per API: `stable`, `unstable` (feature-gated), `internal`.
- A compatibility test suite consumers run against their pinned version
  (`crates/oxibrain/src/compat.rs`).

### 19.3 The Foundation plane

The Foundation v1 contract is a **separate plane** from the durable-memory data plane and
from the agent execution plane. It is not a runtime crate, a daemon broker, or a model
gateway (ADR-007). Its sole job is to give every consumer the same answer to two
questions:

The brain owns `~/.oxi/brain/`, full stop. Hosts that integrate via `oxibrain-client`
spawn a caller-owned stdio child (`LocalProcessEndpoint { executable, dir }`,
`BrainClient::spawn_local`) that speaks JSON-RPC over the child's stdin/stdout.
There is no socket discovery: every caller passes an explicit `--dir`, and the
`handshake` native JSON-RPC method rides the same transport for capability
negotiation (§15.7, `doc/CONSUMPTION_CONTRACT.md`). Hosts never open the store
file, never parse the SQLite WAL, and never read a profile JSON to extract a
secret — the secret stays in the OS Keychain, behind the host's `SecretResolver`
(§15.7, `doc/spec/oxi-foundation-v1.md`).

## 20. Milestones

M0–M6 have shipped. `doc/ROADMAP.md` carries M7 onward with exit criteria and effort.

| Milestone | Content | Status |
|---|---|---|
| **M0** | Foundation: store, migrations, writer actor, canonical serialization, content-derived ids, ports with fakes, CI | ✅ |
| **M1** | Knowledge core, fully deterministic, no model: registry, entities, statements, assertions, the fold, contradictions, resolution, reprojection | ✅ |
| **M2** | Retrieval and lifecycle: indexes, hybrid query, traversal, decay, compaction, communities, context assembly, benchmarks | ✅ |
| **M3** | Extraction and evaluation: LLM port + HTTP adapter, generated schema, validator, quarantine, re-extraction, eval harness; v2.11 swaps the durable job queue for the derived `uncached_memory_episodes` backlog (§9.1) | ✅ |
| **M4** | Surfaces and security: spaces, scopes, tokens, audit, trust tiers, redaction, MCP server, foreground `serve --http` + caller-owned `serve --stdio`, CLI, export/import | ✅ |
| **M10** | **Honest memory** — P10, MMR, reranking, feedback | → ROADMAP |

M7–M10 each get an implementation spec in `doc/spec/` before code, following the pattern M1–M3
established.

---

## 21. Implementation status

The gap between this document and the tree, as measured at `main@cc584b7`. Every entry is
verified by file and line; F22–F26 by compiling and running the code, the rest by reading.
`doc/ROADMAP.md` says when each is closed.

| ID | Finding | Location | Closed by |
|---|---|---|---|
| F1 | `hybrid_query` never reads `as_of` / `min_confidence` | `store/query.rs:339–506` | M8 |
| F2 | `dropped` never populated | `store/query.rs:348` | M8 |
| F3 | context assembly is lexical-only | `store/context.rs` | M8 |
| F4 | `PinnedFacts` empty, `QueryNeighborhood` skipped | `store/context.rs` | M8 |
| F5 | `RecallHints` alters one integer | `store/context.rs` | M8 |
| F6 | belief rendering drops the subject | `store/context.rs::render_belief` | M8 |
| F7 | compression policy inverted | `store/context.rs` | M8 |
| F8 | `transaction_at` ignored | `store/query.rs::beliefs_as_of` | M8 |
| F9 | no chunking | — | M8 |
| F10 | no reranking | — | M10 |
| F11 | traversal filters ignored; adjacency from `statements`, not `beliefs` | `store/query.rs` | M8 |
| F12 | no blocking; full type scan per mention | `store/knowledge.rs:165` | M9 |
| F13 | `graph_context` hardcoded `0.0` | `store/project.rs:105` | M9 |
| ~~F14~~ | ~~`w_alias` dead field~~ — removed, replaced by `w_ngram` | `core/resolution.rs` | ✅ M7 |
| ~~F15~~ | ~~no `EmbeddingPort` implementation exists~~ — `oxibrain-embed-local` (BGE-M3) implements it | workspace-wide | ✅ M7 |
| ~~F16~~ | ~~dense search branch is a comment~~ — `dense_search` + `QueryMode::Dense` | `store/query.rs` | ✅ M7 |
| ~~F17~~ | ~~`upsert_vector` has no production caller~~ — wired into reproject | `store/vectors.rs` | ✅ M7 |
| ~~F18~~ | ~~vectors inside the byte-identical snapshot~~ — split into truth/ranking | `store/index_ops.rs` | ✅ M7 (structural; tolerance pending 7.3) |
| F19 | `fabricated_entity_rate` hardcoded to `0.0` | `core/eval.rs` | M10 |
| F20 | label propagation ignores belief confidence | `index/community.rs` | M10 |
| F21 | facade holds ≈370 LOC of pipeline logic | `oxibrain/src/lib.rs:751,1033,1153` | M8–M10 |
| ~~F22~~ | ~~FTS configured `porter unicode61`~~ — replaced by `unicode61` + `trigram` (v6) | `migrations/v3.sql:7` | ✅ M7 |
| ~~F23~~ | ~~hardcoded English stopword list~~ — deleted, n-gram features | `index/vector.rs` | ✅ M7 |
| ~~F24~~ | ~~`s.len() > 1` filters on **bytes**~~ — deleted, n-gram features | `index/vector.rs` | ✅ M7 |
| ~~F25~~ | ~~Chinese/Japanese sentence → 1 token~~ — model tokenizer (7.1) + n-gram fallback (7.11) | measured, §7.1 | ✅ M7 |
| ~~F26~~ | ~~Korean tokens carry agglutinated particles~~ — model tokenizer (7.1) | measured, §7.1 | ✅ M7 |
| ~~F27~~ | ~~`estimate_tokens = chars/4`; CJK context-window overflow~~ — `TokenizerPort` exact counts (7.1, 7.4) | `core/context.rs` | ✅ M7 |
| ~~F28~~ | ~~`jaro_winkler` prefix bonus boosts shared surnames~~ — replaced by n-gram Jaccard | `core/resolution.rs` | ✅ M7 |
| F29 | MCP `search`/`traverse` never exposed `as_of` / `min_confidence` — so adding them is additive | `mcp/server.rs::tool_list` | M8 |
| F30 | `recall`'s advertised description promises layers that are never populated | `mcp/server.rs::tool_list` | M8 |

---

## 22. Risks

| Risk | Mitigation |
|---|---|
| **Local extraction quality is poor** | The §17.2 comparison splits by extractor, so this is diagnosed rather than guessed. Mitigation is tiering (§8.5). If tier 0 is unusable even with grammar constraints, C2 degrades to "no API key needed for retrieval" — a smaller promise, made honestly |
| **The (c) − (b) delta is small** | Pre-committed response in §23, D19. Not a late discovery |
| Bundled C++ build burden | We already bundle C for SQLite. `candle` / `mistral.rs` re-examined at the M7 gate; it is an adapter swap |
| Trigram index growth | Measured in M7; mitigation is chunk-level-only n-gram indexing, never script routing (§7.4) |
| Extraction quality is mediocre and the graph is noise | M1–M2 are useful with manual writes alone; eval gates; quarantine keeps noise out of beliefs; re-extraction makes upgrades free |
| Scope for a solo developer | Each milestone has standalone exit criteria; stopping between any two leaves a coherent system |
| SQLite becomes the bottleneck | The graph layer is projection: adjacency can move engines without touching the ledger |
| Community layer reintroduces non-determinism | `Derived` is terminal, text is cached, clustering pins `as_of`; the reprojection test fails loudly if that regresses |
| Over-abstraction slows progress | Ports ship with fakes and pay for themselves in tests; `BlobPort`, `Set{max}` cardinality, transitive closure and best-first traversal were all cut for lack of evidence |

---

## 23. Decision log

**D1 — Rust-native engine, not a Graphiti wrapper.** A Python service plus Neo4j/FalkorDB
contradicts the standalone, single-binary, embeddable requirement.

**D2 — oxibrain owns its storage.** Sharing one file across crate boundaries with no migration
contract is a corruption path.

**D3 — Assertion log instead of versioned edges.** Costs one join on read; buys 1:N provenance,
corroboration confidence, real transaction time, non-destructive retraction, and reversible
resolution.

**D4 — Deterministically derived identity, not random ULIDs.** Episode ids derive from source
occurrences; entity ids derive from first-mention location. Both are acyclic and replay-stable.

**D5 — `EpisodeKind::Derived` is terminal.** Otherwise the community layer creates a
generate→extract→recluster→generate loop and destroys reprojection determinism.

**D6 — The ledger is the only durable write path.** Manual writes become `Declaration` episodes.
Otherwise reprojection erases exactly the knowledge the user cared most about.

**D7 — Sentinel timestamps, never NULL.** Fixes a real primary-key defect and removes NULL
branching from the interval algebra.

**D8 — Registry major/minor versioning.** Adding a predicate, a `profile_relevant` flag, or an
affix list must not force a paid re-extraction of the corpus.

**D9 — Deterministic knowledge core before any model; retrieval and lifecycle next.** Debugging
a fold bug and a model bug simultaneously is how these projects die.

**D10 — Forgetting never deletes; consolidation writes derived episodes.**

**D11 — One writer per store, scoped to one operation.** v2.11 reversed D11's earlier
text ("daemon as the default multi-app topology"): two processes with independent
in-memory indexes over one SQLite file is still a corruption path, but the answer is
**operation-scoped handles** (P8), not a resident daemon. Multiple applications share
a brain without a daemon because **no application monopolizes the lock**.

**D12 — SQLite, and no embedded graph database.**
candidate, is archived; Cozo and SurrealDB forfeit FTS5, `sqlite-vec`, the online backup API and
WAL semantics this design leans on. And P1 makes the choice reversible: the graph layer is
projection, so adjacency can move engines without touching the ledger. **Every reviewed system
that uses a real graph store is server-shaped**, which is the shape we are not.

**D13 — Community layer via label propagation, in the consolidation window.** Leiden re-clusters
the world; this graph grows continuously.

**D14 — MCP sampling as a gated provider.** Adopted for onboarding; **retained for quality**
after C2 removed the onboarding problem (§8.3). Separate capability, off by default, per space,
audited.

**D15 — Reject LLM-chosen destructive updates.** An LLM that picks ADD/UPDATE/**DELETE**/NOOP
against stored memories erases without record. oxibrain appends a denying assertion and lets the
fold decide. **A model never holds the delete key.**

**D16 — Budgets, not commitments, for performance.** Each may be revised once with recorded
evidence, then becomes a gate.

**D17 — Depend on `oxicode-ai`, not `oxicode-sdk`, and only through a port.** And it must be
optional, or "standalone" is a lie.

**D18 — The projection splits into a truth half and a ranking half.** Real embeddings are not
bit-reproducible; P1 as written promised more than the design needs. Extending §11.7's existing
truth/salience line beats bolting an exception onto P1, because exceptions accumulate and
extensions do not. Tolerance calibrated in M7, not guessed.

**D19 — Retrieval is `targets × channels × fusion × rerank × filters`; modes become presets.**
Moving filters into a typed struct consumed by a pure ranker makes it structurally impossible to
silently ignore `as_of` in three executors, which is what v1.0 did. *If the §17.2 delta proves
small, the pre-committed response is to demote the graph from query structure to ranking signal
— which `Rerank::GraphDistance` already makes a configuration change rather than a rewrite —
while keeping the truth half, which no control arm can offer at any benchmark score.*

**D20 — Views are rendered, never stored.** A stored page can disagree with the fold, recreating
the invalidation problem the architecture exists to avoid.

**D21 — The profile is a standing query over beliefs, not a new store.** Query-independent facts
are a structural blind spot in pure-search architectures; rendering keeps provenance and validity
on every line.

**D22 — Contextual chunk prefixes are generated from the projection, not from a model.** We
already know each span's entities, predicates, time, source and community. The clearest case
where owning a knowledge graph pays a retrieval dividend that pure-RAG buys with tokens — and it
behaves identically in every language.

**D23 — Uncertainty blocks are computed from the fold, not written by the model.** They must not
go stale when a cached summary does, and must not be something a model can be persuaded to omit.

**D24 — MMR is adopted.** The design has cited the similarity-clustering forgetting problem since
v1.0 while shipping a ranker with no diversity term.

**D25 — Vectors are quantized by default.** Far smaller, Hamming-fast, and much more stable
across backends — which reduces, though does not eliminate, the divergence D18 accounts for.

**D26 — Evaluation reports controlled deltas, not absolute scores.** The number that says whether
this architecture is worth its cost is (c) − (b), not (c).

**D27 — oxibrain owns its model (C2).** Local inference and a local multilingual encoder are the
defaults; providers and MCP sampling are optional quality tiers. The wall a local-first tool
cannot afford is an API key before the product does anything. Because inference sits behind
`LlmPort`, the engine choice stays reversible — the same argument as D12.

**D28 — Grammar-constrained decoding is the primary extraction mechanism, not the fallback.**
Owning the sampler makes schema validity a token-level guarantee, which removes the repair loop,
narrows `extraction_failures` to semantic failures, and is what makes a small local model a
viable extractor. The grammar is generated from the registry, like the JSON Schema (P4).

**D29 — Language independence by construction, never by support matrix (C3).** Character n-grams
for lexical, multilingual embeddings for semantic, rank fusion instead of script routing, the
model's own tokenizer for budgets, affixes as registry data.

**D30 — Jaro-Winkler is removed from name scoring.** Its prefix bonus rewards shared prefixes,
which in surname-first orders means it boosts every pair sharing a surname — the exact confusion
§10 exists to prevent. Replaced by n-gram Jaccard, which shares shingles with the lexical index
and the blocking index.

**D31 — Language independence is tested by writing-system property, not by language.** Seven
representatives cover seven breaking properties; the gate is a 10pp variance bound. A
per-language list is always incomplete; a property list fails loudly when someone adds an
English-shaped optimization.

**D33 — Pull connectors derive identity from `(source_id, locator, predecessor,
content_hash)`.** For push connectors (chat, declarations, agent traces) the call site
supplies identity. The pull connector has no call site — the user owns the files (§1.4) —
so the only inputs it can hash are the source registry, the locator (relative vault path),
the previous occurrence's `occurrence_id`, and the bytes themselves. Hashing `predecessor`
is what makes A → B → A three events rather than two. Hashing `source_id` and `locator`
is what makes equal bytes from different vaults remain independent episodes (§5.6).
Legacy v9 episodes (`source_id IS NULL`) participate in `Unchanged` classification only;
they are never re-ingested because re-ingesting them under a freshly-allocated source
would silently rewrite history. The first `Modified` ingest on the event path for a
previously legacy-only locator migrates it forward, one file per sync, which is the only
point at which legacy disappears.

**D34 — Split is the inverse of Merge; it sets `undone_at` rather than deleting the merge
record.** `Split` (the `oxibrain entity split` CLI verb in §4.2.2, §16.4) does **not** remove
the `entity_merges` row. The operation appends a `Declaration` episode carrying the `Split`
variant, whose projection arm sets `entity_merges.undone_at = now` on the targeted merge and
clears `entities.merged_into` on the previously-losing entity. The merge stays in the ledger
as a recorded past decision, so reprojection can reproduce the historical state of the
system. This is the same write pattern P5 uses for forgetting — redaction is the only
destructive path (§15.5) — and it is the precondition for D6 (the ledger as the only
durable write path): without it, a split would be an out-of-band fix that the next
`reproject` would silently undo.

**D32 — This document is `ARCHITECTURE.md`.** "DESIGN.md" has come to denote a front-end
design-system document; `ARCHITECTURE.md` is the Rust-ecosystem convention and is unambiguous.

---

## 24. Rejected alternatives

Considered against working implementations, recorded so they are not re-litigated.

| Rejected | Considered from | Why |
|---|---|---|
| Multimodal / OCR / page-image embedding | morphik-core | §2 non-goal. GPU-class Python pipeline; incompatible with one binary. Connectors pre-transcribe |
| LLM-chosen deletion / "automatic forgetting" | mem0, supermemory | **D15.** A delete with no record of what was deleted is the opposite of auditability |
| Cloud / multi-tenant service | supermemory, autoflow, morphik | §2 non-goal, and the differentiator |
| Materializing a wiki as files | LLM-Wiki, memory-as-filesystem | Violates §1.4, conflicts with P1. §14 delivers the affordance without the maintenance debt |
| Embedded graph database | Kùzu, Cozo, SurrealDB | **D12** |
| DSPy-style prompt compilation as a dependency | autoflow | Python. §9.6 keeps the idea and drops the dependency |
| A large MCP tool surface | Stash (28 tools) | Navigability, not tool count. **Fifteen is the cap** |
| **Driving the user's agent CLI for inference** | tolaria (`claude_cli.rs`, `codex_cli.rs`, …) | **C2.** Reasonable for an editor whose value is the files. For a memory engine it makes the core capability contingent on other software being installed, and makes extraction quality vary by which CLI is present |
| Per-language tokenizers behind a detector | the standard CJK recipe | **C3 / §7.2.** Detection fails on short, mixed and code-heavy text; unbounded maintenance; discontinuous behaviour. Fusion replaces routing (§7.4) |
| Owning the editor | tolaria, Obsidian | §1.4. Editor-agnosticism makes their users our users |

---

## 25. References

**Architecture**
- Zep / Graphiti — *A Temporal Knowledge Graph Architecture for Agent Memory*, arXiv:2501.13956.
  Bi-temporal edges, episode subgraph, label-propagation communities, invalidate-don't-discard.
  Closest prior art; its search-config separation informs §11 and its MinHash blocking §10.1.
- mem0 — arXiv:2504.19413. LLM-chosen ADD/UPDATE/DELETE/NOOP. Studied and departed from (D15).
- Letta / MemGPT — self-editing memory blocks, no graph. §12 borrows the token-budget framing
  without the self-editing.
- Microsoft GraphRAG — arXiv:2404.16130. Community summarization, local/global split (§11.6).
- Microsoft Memora — value / abstraction / cue-anchor separation; the source of §9.3's framing
  that index entry points and stored content are different things.
- *Retrieval as Reasoning: Self-Evolving Agent-Native Retrieval via LLM-Wiki*, arXiv:2605.25480.
  The navigation-versus-structure ablation behind §14.
- *Memory is Reconstructed, Not Retrieved*, arXiv:2606.06036. The framing behind §12.1.
- *Control-Plane Placement Shapes Forgetting*, arXiv:2606.15903. "Instrument what you discard"
  (§16.3) and P5's explicit-retention argument.
- *MemDelta: Controlled Baselines and Hidden Confounds in Agent Memory Evaluation*,
  arXiv:2606.29914. The reason §17.2 reports deltas.
- Anthropic — *Contextual Retrieval*. The −35 / −49 / −67% figures behind §9.3 and §11.4.
- Think-on-Graph — model-driven iterative traversal; basis for §11.5.

**Foundations**
- Snodgrass, *Developing Time-Oriented Database Applications* — valid vs. transaction time.
- Event sourcing / CQRS — the ledger-and-projection split (P1).
- Cormack et al. (2009) — Reciprocal Rank Fusion.
- Carbonell & Goldstein (1998) — Maximal Marginal Relevance (D24).
- Broder et al. (1997) — MinHash / LSH (§10.1).

**Evaluation**
- In-repo golden corpus + parity corpus (§17.1) — the controlled comparison runs on our own data.
  External benchmarks (LongMemEval, LoCoMo, BEAM) are removed from the plan; the negative/uncertain
  predicate family (§5.5) is still seeded for the provenance work that motivated it.

**Platform**
- Model Context Protocol — long-running tasks, sampling, transport-neutral subscriptions.
- llama.cpp GBNF grammars — the mechanism behind D28.
- Binary quantization (Qdrant, SimSIMD lineage) — D25.
- SQLite FTS5 `trigram` tokenizer — §7.4.
- Loro — Rust CRDT for the post-v1 sync path (§15.6).
