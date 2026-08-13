# Handoff — M9 shipped → gate (golden-only) + remaining

> **Read this first.** This is the handoff for what remains after M9's core landed.
> **Branch:** `main` · working tree clean · `cargo test --workspace` green · `clippy -D warnings` clean.
> **Predecessor:** `2026-08-13-m8-complete-next-gate-m9.md`.
> **Decision this session:** LongMemEval is **removed from the plan**. The gate runs on the
>   in-repo golden corpus only — "complete our implementation", no external benchmark.

---

## State right now (verified)

Seven commits landed this session (`88ca289` → `7cf4f1f`):

| Commit | Scope |
|---|---|
| `88ca289` | §5.1 ranking-tolerance calibration — measured CPU vs Metal, tolerance **2pp** (the floor) |
| `7e6092d` | chunks table populated during projection (gate arm (b) prerequisite) |
| `9610833` | `oxibrain-views` crate + `Brain::brief`/`navigate` (zero-dep renderer, `entity://` links) |
| `55734c8` | CLI `oxibrain page <entity>` + MCP `brief`/`navigate` (fifteen-tool cap held) |
| `eaf45e1` | resolution scaling: `index::blocking` (MinHash/LSH + entropy gate), graph context, PerType weights |
| `6d89a2f` | desktop UI: `brief` view + clickable links; api.ts `brief`/`navigate` |
| `7cf4f1f` | `eval/golden/` skeleton — manifest + format + 2 episodes + 2 questions |

`cargo test --workspace` → 0 failed · `clippy -D warnings` clean · `fmt --check` clean ·
standalone guarantee (`--no-default-features --features http-llm`, no oxi crates) holds.

---

## What's next, in dependency order

### 1. Golden corpus population (data, ~2 days)

`eval/golden/` is a **skeleton** (format + 2 episodes + 2 questions). The gate needs the real
corpus (~200 episodes across note/document/agent-trace, ~100 questions). The format is fixed in
`manifest.toml` + the two episode/question examples — populate it, don't redesign it.

- Categories that matter: `knowledge_update`, `temporal_reasoning` (§17.2).
- Each question carries `answer` + `supporting_episodes` (the corpus is self-grading).

### 2. The gate runner — three-arm, golden-corpus only (code, ~3–4 days)

The three arms, all **buildable today**:
- (a) full context, no retrieval — ceiling.
- (b) lexical + dense chunks + RRF, **no graph** — `Retrieval::lexical` + the now-populated
  `chunks` table; the control.
- (c) oxibrain complete — `Retrieval::hybrid`; the treatment.

Report **(c) − (b) per category**, with tokens/query alongside. Pre-commit the three outcomes
(already in `ROADMAP.md` §4): delta on temporal categories → proceed; delta small with a weak
local extractor → fix extraction, decide nothing structural; delta small with a strong extractor
→ D19's demote (graph → ranking signal; `Rerank::GraphDistance` is a config change).

Concrete shape: extend `oxibrain eval` with a `--suite gate` that loads `eval/golden/`, runs the
three arms against each question, evaluates answers (exact-match against `answer` first; an LLM
judge only if exact-match proves too brittle), and prints the per-category delta table.

### 3. `brief(topic | space)` (code, ~1–2 days)

Only `brief(entity)` is implemented (the M9 exit criteria are all entity-based). §14.1's
`brief(entity | topic | space)` signature still has two arms to fill:

- **space** → `Brain::brief` dispatch for a `space://` target: `stats()` + `list_entities()` as
  followable links. Cheap.
- **topic** → a keyword target: lexical `query()` over entities, rendered as a list of links.

The MCP `brief` tool takes `entity_id` today; add a `target_kind` discriminator (`entity | space
| topic`) — purely additive, still inside the fifteen-tool cap.

### 4. Known gaps (small, not gate-blocking)

- **brief timeline section renders raw entity ids.** `timeline::TimelineEntry.object_repr` is
  `object_entity` (raw id) for entity objects. `store::brief` resolves surfaces for beliefs/
  neighbours/contradictions but reuses `timeline()` verbatim for the timeline section. Resolve
  the surface there too (either post-process in `entity_brief`, or extend the timeline module).
- **Resolution LSH index is built per call.** `block_candidates` builds the `LshIndex` from
  `find_keys_for_type` every resolution — correct candidate generation, but the build is O(N) so
  it is not yet *amortized* sublinear per mention. Cache the index (per space+type) across the
  projection batch, or persist band→key buckets, to make the §5.2 "sublinear per mention" claim
  measured rather than structural.
- **`embedding_sim` is zero during projection.** Resolution runs before `embed_entities` applies
  dense vectors post-projection, so the wired closure returns 0.0 (documented in code). PerType
  weights are set (Person/Org 0.1, Concept 0.6, default 0.3); making the signal non-zero needs a
  decision on resolution-time embedding vs the deterministic-truth contract (P1).

### 5. M9 exit criteria not yet measured (verification, ~1 day)

The code is there; the *measurements* are not:
- 3-hop navigation from one `brief` with only `navigate`, no `search` (needs a running MCP server
  + a client driving it).
- tokens/answer **decrease** vs the `recall`-only path.
- sublinear resolution over a 10⁴-entity fixture (measured, not asserted).
- ≤10pp resolution F1 across writing-system property classes (`eval/parity` suite exists).
- `brief` p95 < 100 ms on the standard fixture.

---

## Repo hygiene (unchanged)

- One conventional commit per change, English.
- `oxibrain-views` has **zero dependencies** — the §18 rule-4 "must not name rusqlite" boundary is
  structural now.
- `oxibrain-store` must not name `rank`/`pack`/`step` (unchanged).
- Fifteen MCP tools is the cap: `brief`+`navigate` replaced `get_entity`+`timeline`.
