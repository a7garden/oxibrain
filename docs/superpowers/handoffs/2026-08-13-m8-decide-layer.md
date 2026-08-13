# Handoff — M8 The Decide Layer (next milestone)

> **Read this first.** This is the handoff for the NEXT task: M8, "the decide layer".
> **Predecessor:** `2026-08-13-m7-model-tasks.md` (M7 fully complete).
> **Branch:** `main` · working tree clean.
> **Spec:** `doc/ROADMAP.md` §3 · **Design:** `doc/ARCHITECTURE.md` §11.2–11.8, §12.1–12.3, §5.7, §9.3, §6.1.
> **Effort:** ≈20–25 days.

---

## What M8 is

> **Goal: filters cannot be silently ignored, and `recall` returns something worth reading.**
> **Delivers P9 for retrieval and context.**

M7 shipped the *model* (local inference, embeddings, dense path). M8 ships the *decide layer*:
the pure `rank` and `pack` functions that P9 demands — so filters cannot be dropped silently,
and `recall` composes Profile + beliefs-with-subjects + neighbourhood + sources within budget.

It is the largest refactor remaining. The old `Query`/`QueryMode`/`RankingResult` types in
`core/retrieval.rs` are superseded by the new `Retrieval` type; the store becomes mechanical
channel execution.

## Current state (verified)

- **`core/retrieval.rs`** has the pre-M8 types: `Query`, `QueryMode` (Hybrid/Lexical/LexicalVector/Dense/Graph/Community), `SearchHit`, `SearchTarget`, `RankedItem`, `RankingResult`, `DroppedItem`, `DropReason`, `TraversalSpec`, `Strategy`, `TraversalResult/Node/Edge`.
- **No `rank`, `pack`, or `step` module exists yet** in `oxibrain-core` — M8 creates them.
- **Facade `crates/oxibrain/src/lib.rs` is 1407 lines — ALREADY under M8's 1,500 exit target.** The ROADMAP's "from 3,067" is stale; verify before chasing it.
- **Schema version is 7** (v7 = 1024-dim `entity_vectors`, M7). **M8's chunks migration in ROADMAP §3.1 (8.11) says "migration v7" — that label is taken; it becomes v8.**
- `hybrid_query(conn, q, embedder: Option<&dyn EmbeddingPort>)` — 3 callers (facade `query()`, `store/context.rs` `assemble_context` passes `None`, bench).
- `store/context.rs::assemble_context` currently does inline packing with `tokenizer.count()` (M7.4 wired the tokenizer). It will be replaced by `core::pack`.

---

## Work order (tree stays green, each = one commit)

M8's tasks are listed in ROADMAP §3.1. Recommended order — **pure core first, then store, then facade** — so the tree never has a red suite:

1. **8.2 + 8.1** — Define the `Retrieval` type (§11.2) and `core::rank(RetrievalInput, &Retrieval) -> RankingResult` (§11.3). These are PURE (no rusqlite, no tokio — same rule as `fold`). Presets `Retrieval::hybrid()`, `::lexical()`, `::semantic()` (dense), `::graph()`, `::community()`.
   - `rank`'s three post-conditions are the contract (property-tested in 8.4): **conservation** (`items ∪ dropped` = candidates, disjoint), **filter totality** (no item violates `spec.filters`), **determinism** (tie-break order).
   - `Filters` is NOT optional — there is exactly one place `as_of`/`known_at`/`min_confidence` can be forgotten, and it has a test.
2. **8.7** — `core::pack(ContextInput, &Budget, &PackPolicy) -> ContextResult` (§12.3), pure. Post-conditions: `total_tokens ≤ budget` counted with `TokenizerPort`; Profile never squeezed out by `reserve`.
3. **8.4** — Property tests for `rank` (conservation/filter-totality/determinism). This is the structural guarantee that makes F2 (`why --dropped`) honest.
4. **8.3** — `store::retrieve`: execute each `Channel`, batch ONE `TargetFacts` query, hand over `RetrievalInput`. Filters that push down cheaply (space, entity type) go into SQL; the fold-dependent ones (`as_of`, `known_at`, `min_confidence`) stay in `rank`.
5. **8.5, 8.6** — Belief-filtered adjacency (F11); `known_at` transaction-time filter (F8).
6. **8.8, 8.9, 8.10** — Profile layer + `profile_relevant` registry flag (D21, minor version — invalidates zero cached extractions); `render_belief` rewrite (F6); expansion policy (F7).
7. **8.11** — `chunks` table + **migration v8** + recursive splitting + deterministic context prefix (§5.7, §9.3, D22).
8. **8.12** — MCP: add `as_of`/`known_at`/`min_confidence` to `search`/`traverse` (purely additive, F29); fix `recall` description (F30).
9. **8.13** — `oxibrain why --dropped` reads real data (F2) — now that `rank` guarantees conservation, dropped items are real.

### The ROADMAP order vs. dependency reality

ROADMAP lists 8.1 first (rank) then 8.2 (Retrieval type). `rank`'s signature references `Retrieval`, so define the type first (or together). The listed days are estimates; the dependency chain is: **8.2/8.1 → 8.4 → 8.3 → 8.5/8.6 → 8.7 → 8.8/8.9/8.10 → 8.11 → 8.12 → 8.13**.

---

## Key spec sections

| Ref | Content |
|---|---|
| §11.2 | `Retrieval` type: targets × channels × fusion × rerank × filters; `Channel`/`Rerank`/`Filters` enums; presets |
| §11.3 | `RetrievalInput` + `rank()` pure; three post-conditions; store is mechanical |
| §11.4 | Rerankers: Corroboration (free, Support already stored), GraphDistance, Mmr (O(k²)), CrossEncoder |
| §11.5 | Traversal + why `as_of` is free |
| §11.8 | Explainability |
| §12.2 | Layers + `profile_relevant` registry flag; profile is a standing query, not a new store |
| §12.3 | `ContextInput` + `PackPolicy` + `pack()` pure; compress-by-default, expand top-k |
| §5.7 | Schema sketch (chunks table) |
| §9.3 | Chunking with a deterministic context prefix |
| §6.1 | Two time axes: valid time (`as_of`) vs transaction time (`known_at`) |
| §17.4 | Property-test expectations |

---

## M8 exit criteria (ROADMAP §3.2)

- [ ] `search(as_of = 2025-03-01)` returns a different result set than `search()` on a fixture where beliefs changed — **this test fails today in three separate executors**
- [ ] `traverse(depth=2, min_confidence=0.8, valid_at=t)` excludes retracted edges
- [ ] `why --dropped` prints a non-empty, correctly-attributed list
- [ ] Property test: `items ∪ dropped` = candidates, disjointly
- [ ] `recall` returns Profile + beliefs-with-subjects + neighbourhood + sources within budget; a human can tell what the brain knows
- [ ] MCP contract test: a v1.0-schema client still works unmodified
- [ ] Facade under 1,500 LOC (currently 1,407 — already met)

---

## Critical context / gotchas

- **P9 is enforced by boundaries** (AGENTS.md): `oxibrain-store` must NOT name `rank`, `pack`, or `step` — it may name their input/output types. Pure decision modules live in `core`.
- **Schema v7 is taken.** 8.11's chunks table is **v8**. Update the migration test (`expected: 8`) and `LEDGER_SCHEMA_VERSION` when you add it.
- **`Query`/`QueryMode` → `Retrieval`:** the MCP API strings (`hybrid`, `lexical`, `lexical-vector`, `dense`, `graph`, `community`) must keep working — presets map them, so the MCP surface is stable (F29 additive).
- **`assemble_context` is called from the facade (2 methods), MCP, bench, tests** — when `core::pack` replaces its inline packing, thread the tokenizer (already in `Brain`) and keep the public signature stable.
- **`render_belief` (F6)** currently drops the subject — the rewrite must include subject + canonical key + validity + support.
- **Corroboration reranker is "free"**: `Support { distinct_episodes, … }` is already computed and stored but affects nothing in ranking yet.
- **`step`** is referenced in AGENTS.md boundaries ("Do not name `rank`, `pack`, or `step` in `oxibrain-store`") — M8 introduces it (the fold/processing step that produces `RetrievalInput.facts`). Confirm the intended shape from §11.3 before implementing.

---

## After M8: the gate (ROADMAP §4)

Three-arm comparison (full-context vs lexical+dense+RRF vs oxibrain complete) under both
extractors (local + frontier) on LongMemEval + golden corpus. ~5 days. Runs on M8 exit.

## Also still open from M7

- **§5.1 ranking tolerance calibration** (the one open M7 checkbox): create `eval/probes/`,
  measure recall@10 on CPU + Metal (ten runs each), set tolerance
  `max(2pp, 2 × observed_max_delta)`, write the number + date into ARCHITECTURE.md §5.1.
  Independent of M8 — can be done in parallel.
