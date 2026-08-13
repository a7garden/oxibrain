# Handoff — M9 shipped → gate (golden-only) + remaining

> **Read this first.** This is the handoff for what remains after M9's core landed.
> **Branch:** `main` · working tree clean · `cargo test --workspace` green · `clippy -D warnings` clean.
> **Predecessor:** `2026-08-13-m8-complete-next-gate-m9.md`.
> **Decision this session:** LongMemEval is **removed from the plan**. The gate runs on the
>   in-repo golden corpus only — "complete our implementation", no external benchmark.

---

## State right now (verified)

**All five remaining-work items shipped this session** (`29a4c08` → `52e472c`, on top of the
seven M9 commits `88ca289` → `7cf4f1f`):

| Commit | Scope |
|---|---|
| `29a4c08` | small gaps 4a/4b: brief timeline surfaces + `ResolutionCache` (per (space,type) LSH index, threaded through project/reproject/extraction) |
| `17c6da4` | ADR-004 — `embedding_sim` = 0.0 during projection (documented decision + re-evaluation triggers) |
| `c2bc7fa` | `brief(space|topic)`: `BriefTarget` enum, `Brain::brief_target`, space/topic view renderers, MCP `target_kind` discriminator, CLI `--kind` |
| `e4a9221` | gate runner `oxibrain eval --suite gate` + starter corpus (10 ep / 10 q, en/ko/zh) + FTS/TF-IDF exclude Declaration episodes (JSON pollution fix) |
| `c92a399` | gate measures M8 recall-only vs M9 brief/navigate tokens/answer |
| `95f8b7f` | M9 exit: 3-hop MCP navigation test + brief p95 bench + resolution scaling test |
| `850610c` | M9 exit: resolution F1 ≤10pp across 7 writing systems (parity_corpus) |
| `52e472c` | resolution scaling marked `#[ignore]` (measurement, not CI gate) + bench clippy fix |

`cargo test --workspace` → 62 targets green · `clippy -D warnings` clean · `fmt --check` clean ·
standalone guarantee (`--no-default-features --features http-llm`, no oxi crates) holds.

---

## What's next, in dependency order

### 1. Golden corpus population (data, the only remaining ~2-day item)

`eval/golden/` has a **starter corpus** (10 episodes, 10 questions across en/ko/zh, both
categories). The gate runs end-to-end on it; the full ~200-episode / ~100-question corpus for a
*statistically meaningful* delta is human-curated content, added incrementally. Format is fixed
in `manifest.toml` + the episode/question examples — populate, don't redesign.

- Categories that matter: `knowledge_update`, `temporal_reasoning` (§17.2).
- Each question carries `answer` + `supporting_episodes` (the corpus is self-grading).
- Predicates must exist in the core/v1 registry (`leads`/`reports_to`/`job_title`/`lives_in`
  were renamed to `works_on`/`knows`/`has_skill`/`located_in` to keep the corpus ingestible).

### 2. Persist the resolution cache on Brain (code, ~1 day) — **the measured gap**

The resolution-scaling measurement (in `m9_resolution_scaling.rs`) shows per-entity cost is
flat (~11 µs — LSH blocking sublinear), but per-declare total grows ~linearly with N because
`Brain::declare` builds a fresh `ResolutionCache` per call. Sublinear **within** a reproject
batch (shared cache), linear **across** incremental declares. Making the cache a field on Brain
(Mutex<ResolutionCache>) closes it — the `invalidate` hook already handles staleness on New/
Candidate. The §5.2 "sublinear per mention" claim then holds on the live path, measured.

### 3. Gate outcome decision (verification, ~1 day)

Starter-corpus numbers: **b=10/10, c=10/10, delta 0 pp both categories** (the graph adds no
accuracy at 15 statements — expected), tokens/query b=56 c=117 (+60 for the graph arm),
tokens/answer M8=736 vs M9=482 (−254, the brief path is cheaper). Once the full corpus lands,
re-run and apply ROADMAP §4's pre-committed outcomes: clear delta on temporal → proceed with
M10; small delta with weak local extractor → fix extraction; small delta with strong extractor
→ D19's demote (graph → ranking signal, `Rerank::GraphDistance` config change). Arm (a) ceiling
and the frontier tier remain out of scope for the golden-only gate.

### 4. M9 exit criteria — status

All five are now **measured** (the code shipped in M9; this session produced the numbers):
- 3-hop navigation: `navigate_three_hops_reaches_deep_entity` MCP test passes (brief(Alice) →
  navigate → navigate reaches Bob, no search).
- tokens/answer: **M8 recall-only 736 vs M9 brief/navigate 482 tok/q (−254, −34%)** — decrease
  holds, measured in the gate report.
- sublinear resolution: per-entity ~11 µs flat; **per-declare linear until the cache persists
  on Brain** (item 2) — the honest partial state.
- F1 parity: **F1 = 1.00 on all 7 writing systems, spread 0.0 pp ≤ 10 pp** (parity_corpus).
- brief p95: **~2.5 ms/brief on a 1000-entity fixture**, far under the 100 ms criterion
  (`cargo bench -p oxibrain --bench m9_exit`).

---

## Repo hygiene (unchanged)

- One conventional commit per change, English.
- `oxibrain-views` has **zero dependencies** — the §18 rule-4 "must not name rusqlite" boundary is
  structural now.
- `oxibrain-store` must not name `rank`/`pack`/`step` (unchanged).
- Fifteen MCP tools is the cap: `brief`+`navigate` replaced `get_entity`+`timeline`.
