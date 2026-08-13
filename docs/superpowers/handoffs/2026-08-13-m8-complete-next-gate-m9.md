# Handoff — M8 complete → Gate + M9 (next tasks)

> **Read this first.** This is the handoff for what comes after M8.
> **Predecessor:** `2026-08-13-m8-decide-layer.md` (M8 fully complete).
> **Branch:** `main` · working tree clean.
> **Spec:** `doc/ROADMAP.md` §4 (Gate), §5 (M9) · **Design:** `doc/ARCHITECTURE.md` §17.2 (gate), §5.1 (tolerance), §14 (navigation).
> **Effort:** §5.1 calibration ≈1–2 days (independent); Gate ≈5 days; M9 ≈15–20 days.

---

## State right now (verified)

M8 is **100% complete and committed**. Eleven commits landed (`781ebe6` → `8d117a8`):

| Commit | Scope |
|---|---|
| `781ebe6` | §8.1+§8.2 — pure `core::rank` + `Retrieval` type |
| `05858ab` | §8.4 — property tests (conservation/totality/determinism); fixed HashMap non-determinism |
| `b63855c` | §8.7 — pure `core::pack` + §12.4 summary-source pairing |
| `a4dd2f5` | §8.5+§8.6 — belief-filtered adjacency (F11) + retract SQL fix + known_at |
| `51ff949` | §8.8-§8.10 — `profile_relevant` flag (CORE_V1_MINOR=1), `render_belief` subject (F6) |
| `ff608a6` | §8.11 — `chunks` table + **migration v8** + recursive splitter + context prefix |
| `710ebae` | §8.12 — MCP additive `as_of`/`known_at`/`min_confidence` (F29) + exit tests |
| `fba9ef0` | §8.13 — `why --dropped` real data (F2) + 4× channel fetch cap |
| `3beb04e` | §3.2 — recall = Profile + beliefs-with-subjects + neighbourhood + sources |
| `8d117a8` | §3.2 — MCP v1.0-schema client contract test |

**All seven §3.2 exit criteria pass** (verified by `cargo test --workspace`, 56 suites):
- `search(as_of)` differs from `search()` — was failing in 3 executors pre-M8 (F1/F3/F11)
- `traverse(depth=2, min_confidence=0.8, valid_at=t)` excludes retracted edges
- `why --dropped` prints a non-empty, attributed list
- property test: `items ∪ dropped = candidates`, disjoint
- `recall` returns Profile + beliefs-with-subjects + neighbourhood + sources within budget
- MCP v1.0-schema client works unmodified
- facade 1,407 LOC < 1,500

`cargo clippy --workspace --all-targets` → 0 warnings · `cargo fmt --check` clean.

---

## What's next, in dependency order

### 1. §5.1 ranking-tolerance calibration (independent, parallel-safe, ~1–2 days)

The **only open M7 checkbox**. `ARCHITECTURE.md §5.1` requires the tolerance be *measured,
not guessed*: `max(2pp, 2 × observed_max_delta)` for recall@10 across CPU vs Metal, using the
shipped quantized encoder, recorded **with the date and the measurement**.

- Create `eval/probes/` — a fixed probe set (episodes spanning writing systems, per §7.8).
- Run recall@10 on CPU and Metal, **ten runs each**, record the observed max delta.
- Write the number + date into `ARCHITECTURE.md §5.1`.
- This is the executable form of the "ranking half is equivalent, not identical" contract (P1).

Independent of the gate — can be run in parallel with it. Nothing after it depends on a guess.

### 2. The Gate — three-arm comparison (ROADMAP §4, ~5 days)

**Not a milestone. A decision point. Nothing after it should be planned in detail before it
returns a number.** M8 exit is the first time arms (b) and (c) are both buildable:

| Arm | Configuration |
|---|---|
| (a) | full context, no retrieval — ceiling |
| (b) | lexical + dense chunks + RRF, **no knowledge graph** — the control |
| (c) | oxibrain complete — treatment |

Run each under **both** extractors (local tier 0, frontier tier 1) on LongMemEval + golden
corpus, reporting tokens/query. The reported quantity is **(c) − (b), per category** (§17.2);
the categories that matter are knowledge update and temporal reasoning.

Three pre-committed outcomes (do not improvise the response — pick the row):

| Outcome | Response |
|---|---|
| Delta clear on temporal categories | Proceed with M9 and M10 as written |
| Delta small, **local extractor is the bottleneck** | Extraction quality is the problem, not the architecture. Fix extraction, re-run, decide nothing structural yet |
| Delta small **with a strong extractor** | **D19's pre-commitment:** demote graph from query structure to ranking signal. Keep the truth half; cut communities to a salience input. `Rerank::GraphDistance` makes this a config change, not a rewrite |

**Publishing the number regardless of direction is the point.** A document that cites MemDelta
and declines to run its own controlled comparison has learned nothing from it.

### 3. M9 — Agent-native (ROADMAP §5, ~15–20 days)

> Goal: an agent can explore the brain instead of being handed a blob. Delivers §14.

| # | Item | Ref | Days |
|---|---|---|---|
| 9.1 | `oxibrain-views` crate — must NOT name `rusqlite` | §14.2, §18 | 2 |
| 9.2 | `brief(entity\|topic\|space)` → markdown with followable links | §14.1 | 4 |
| 9.3 | `navigate(from, link)` | §14.1 | 2 |
| 9.4 | `oxibrain page <entity>` CLI parity | §16.4 | 1 |
| 9.5 | MCP tools 14 and 15 — **the cap** | §16.2 | 1 |
| 9.6 | Determinism test: `brief(e)` twice on unchanged ledger is equal | §14.2 | 1 |
| 9.7 | Resolution blocking: MinHash/LSH over §7.3 shingles + entropy gate | §10.1, F12 | 3–4 |
| 9.8 | Graph context in resolution — `w_graph` stops multiplying zero | §10.2, F13 | 2 |
| 9.9 | `w_embedding: PerType`, now that embeddings exist | §10.3 | 1 |
| 9.10 | Desktop UI: `brief` view + `Retrieval` preset change in `apps/brain-ui/src/api.ts` | — | 2 |

M9 exit criteria: 3-hop navigation with no `search`; tokens/answer decrease vs `recall`;
sublinear resolution on 10⁴ entities; ≤10pp resolution F1 across writing systems; `brief` p95
< 100 ms.

---

## M8 decisions that shape the next work

- **`core::rank`/`pack` are pure and property-tested** (P9). Store is mechanical. The gate's
  arm (b) uses `Retrieval::lexical()` + chunks — already buildable.
- **`Rerank::GraphDistance` is implemented** in `core::rank` (salience fallback) — D19's demote
  path is a config change, exactly as designed.
- **`chunks` table exists at schema v8** but is not yet populated by projection. M9's arm (b)
  "dense chunks" needs the projection to fill it (the splitter + context prefix are in
  `core::chunking`; the projection hook is not wired).
- **`profile_relevant`** drives the Profile layer (§8.8); `Brief` can reuse it.
- **`why --dropped`** reads real `DroppedItem` data — the explainability primitive §11.8 wants.

## Known gaps to close before M9 (not gate-blocking)

- **chunks are not yet populated** — `core::chunking::split_into_chunks` + `render_context_prefix`
  exist and are tested, but no store path writes the `chunks` table during projection. Arm (b)
  of the gate needs this. Wire it (a `reproject`/index-ops step) before running the gate.
- **`Rerank::Mmr` and `Corroboration` are listed in M10** (§11.4) — arm (b) RRF fusion works
  today; the rerankers are M10 scope.

---

## Repo hygiene

- Keep the tree green: each change is one commit (conventional, English).
- Boundaries still enforced: `oxibrain-views` must not name `rusqlite` (§18 rule 4); store must
  not name `rank`/`pack`/`step`.
- Do not plan M9 detail before the gate returns a number (§4). The gate's three outcomes
  change M9's scope; pre-commit the response.
