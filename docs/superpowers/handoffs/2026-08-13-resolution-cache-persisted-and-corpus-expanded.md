# Handoff — M9 resolution cache persisted + golden corpus expanded

> **Predecessor:** `2026-08-13-m9-shipped-gate-and-remaining.md`.
> **Branch:** `main` · `cargo test --workspace` green (350/0/5) · `clippy -D warnings` clean · `fmt --check` clean · standalone guarantee holds.

---

## What shipped this session

### 1. Persistent resolution cache on Brain (the measured gap — closed)

**Problem:** `Brain::declare` built a fresh `ResolutionCache::new()` per call, so the O(N) LSH index was rebuilt for every incremental declare. Per-declare cost grew linearly with N.

**Fix — two changes:**

**(a) Cache persists as `Arc<Mutex<ResolutionCache>>` on Brain.** All four constructors initialise it. `declare` and `extract_one_with` lock it inside the writer closure. `reproject` and `redact` clear it (projection rebuilt or entities removed → stale). The writer actor is single-threaded, so the mutex serialises naturally — no contention, no deadlock.

**(b) Incremental insertion replaces invalidation.** `invalidate` removed the cached entry, forcing a full O(N) rebuild on the next mention of the same type. New `LshIndex::insert` + `ResolutionCache::insert_key` add a single key to the existing index in O(1). `insert_entity_key` now returns `bool` (was the row actually inserted?) so the cache is only updated when a genuinely new key is added. All three resolution decision paths (Link, New, Candidate) call `insert_key`.

**Measured result** (release build, Apple M4):

| N entities | Before (per-call) | After (persistent) | Speedup |
|---|---|---|---|
| 500 | 6.0 ms / 12.0 µs | 2.0 ms / 4.0 µs | 3.0× |
| 1000 | 11.2 ms / 11.2 µs | 3.8 ms / 3.8 µs | 2.9× |
| 2000 | 22.9 ms / 11.5 µs | 7.5 ms / 3.7 µs | 3.1× |

Growth: N×2.0 → time ×1.89 and ×1.96 — both **<2.0, confirming sublinearity**.
Per-entity cost is flat (~4 µs) and decreasing as N grows.
The §5.2 "sublinear per mention" claim now holds on the live path.

**Files changed:**
- `crates/oxibrain-index/src/blocking.rs` — `LshIndex::insert` (O(1) incremental)
- `crates/oxibrain-store/src/knowledge.rs` — `insert_entity_key` returns `bool`
- `crates/oxibrain-store/src/project.rs` — `ResolutionCache::insert_key`, `clear`; `resolve_or_create` uses `insert_key` on all 3 paths
- `crates/oxibrain/src/lib.rs` — `cache: Arc<Mutex<ResolutionCache>>` field on Brain; wired through `declare`/`extract_one_with`; cleared on `reproject`/`redact`
- `crates/oxibrain/tests/m9_resolution_scaling.rs` — updated measurements

### 2. Golden corpus expanded (10 → 25 episodes, 10 → 25 questions)

Added 15 new episodes and 15 new questions covering:
- **Dave** (en): career change Umbrella → Stark Industries, skill Python → Rust
- **Eve** (en): project change Phoenix → Project K, employer Stark Industries
- **박지은** (ko): career change 네이버 → 카카오, knows 김민수
- **王芳** (zh): employer 阿里巴巴, manager change to 王明, knows 张伟
- **Carol** (en): born_in Seoul, moved Berlin → back to Seoul
- **Alice** (en): full_name update → "Alice Wonder"
- **Social network**: Dave knows Carol

New predicates exercised: `full_name`, `born_in`, `has_skill` (with literal objects), `knows`.
Categories: 16 knowledge_update, 9 temporal_reasoning.
Manifest version bumped to 0.2.

### 3. Gate outcome (expanded corpus)

```
Episodes ingested: 32 declarations
Questions:         25

Per-category accuracy (c − b):
  knowledge_update        b=15/16  c=15/16  delta(c−b) = +0
  temporal_reasoning      b=9/9   c=9/9    delta(c−b) = +0

Tokens/answer:  M8 recall-only=902  M9 brief/navigate=538  delta = -363 tok/q (-40%)
```

- **Temporal reasoning: 9/9 pass** for both arms.
- **Knowledge update: 15/16 pass** — only q-002 (Globex) fails for both arms (ranking issue: the Globex statement isn't in top-5 for the "Alice" keyword).
- **Delta (c−b) = 0** for both categories — expected at this corpus size. The graph adds no accuracy at 32 statements.
- **Tokens/answer: M9 brief saves 40%** vs M8 recall-only (538 vs 902 tok/q).

---

## M9 exit criteria — final status

All five measured and closed:

| Criterion | Status | Evidence |
|---|---|---|
| 3-hop navigation | ✅ | `navigate_three_hops_reaches_deep_entity` MCP test |
| tokens/answer decrease | ✅ | M8 902 → M9 538 tok/q (−363, −40%) |
| sublinear resolution | ✅ | ~4 µs/entity flat, growth ×1.89/×1.96 < 2.0 |
| F1 parity ≤10pp | ✅ | F1 = 1.00 on all 7 writing systems |
| brief p95 < 100ms | ✅ | ~2.5 ms/brief on 1000-entity fixture |

---

## What's next

### Gate outcome decision (ROADMAP §4)

At 32 declarations, delta = 0 pp for both categories. ROADMAP §4's pre-committed outcomes:
- **Clear delta on temporal → proceed with M10.** Not met yet (delta = 0).
- **Small delta with weak extractor → fix extraction.** Not applicable (no extractor in the gate; declarations are direct).
- **Small delta with strong extractor → D19's demote.** This is the expected path: the graph is not adding accuracy at this scale, so the graph should be a ranking signal (`Rerank::GraphDistance` config change), not the primary retrieval path.

**The corpus is the bottleneck.** 32 declarations across 25 questions is too small for a statistically meaningful delta. The gate runner code is correct — it just needs more data. The full ~200/~100 corpus is human-curated content added incrementally.

### Remaining corpus needs

- More multi-hop questions where arm (c) can navigate the graph and arm (b) cannot find the answer via lexical/dense alone.
- Contradiction scenarios (two sources disagree; the brain must surface the conflict).
- More entity aliases/surface variations to exercise resolution.
- Entity merges (manual `Declaration::Merge`) and their effect on queries.

### Architecture notes

- `ResolutionCache` is now `Send + Sync` (all fields are owned data: `HashMap<(String, String), (LshIndex, Vec<EntityKey>)>`). The `Arc<Mutex<>>` wrapper is safe because the writer actor processes closures serially.
- The `invalidate` method is retained for batch-import paths that change many keys at once. The resolution path uses `insert_key` (incremental) instead.
- `reproject` uses its own local `ResolutionCache` (separate from Brain's persistent one) for the batch rebuild, then clears the persistent cache on completion.

---

## Repo hygiene

- `oxibrain-views` has zero dependencies (§18 rule-4 boundary is structural).
- `oxibrain-store` does not name `rank`/`pack`/`step`.
- Fifteen MCP tools is the cap: `brief`+`navigate` replaced `get_entity`+`timeline`.
- `cargo build -p oxibrain --no-default-features --features http-llm` passes; no `oxios-`/`oxicode-` crates in the tree.
