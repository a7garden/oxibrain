# oxibrain Roadmap — M7 onward

> **Version:** v1.1 · **Date:** 2026-08-13
> **Status:** Canonical for sequencing. Per-app sequencing lives in `doc/ECOSYSTEM.md`.
> **Scope:** what happens after M6. M0–M6 shipped (`doc/ARCHITECTURE.md` §20).
> **Convention:** `§n` refers to `doc/ARCHITECTURE.md`. `F n` are findings in its §21.

---

## 0. Where we actually are

M0 through M6 landed, then **M7, M8, and M9 shipped** in sequence. `cargo test --workspace`
→ green (350/0/5) · `clippy -D warnings` clean · standalone guarantee (no oxi crates) holds.

- **M7** — own the model: local GGUF inference (CPU/Metal), grammar-constrained extraction, a
  real multilingual embedder, the Truth/Ranking split, and the §5.1 tolerance **measured** (2pp).
- **M8** — the decide layer: pure `rank`/`pack`, belief-filtered traversal, chunks, all seven
  exit criteria verified.
- **M9** — agent-native: `oxibrain-views` (`brief`/`navigate`), resolution blocking (MinHash/LSH)
  + graph context + PerType embedding weights, and a **persistent resolution cache** on `Brain`
  with incremental `insert_key` (sublinear per mention on the live path, measured). All five exit
  criteria closed.

The gate (§4) has been run on the expanded golden corpus (25 episodes / 25 questions / 32
declarations): delta(c−b) = 0 at this scale, tokens/answer M8 902 → M9 538 (−40%). The full
~200/~100 corpus is the bottleneck for a definitive gate decision. **M10** (§6) is the next
milestone.

---

## 1. How this is sequenced

Four rules govern the order.

**1. Unblock measurement before adding capability.** Every decision after M8 depends on the
three-arm comparison (§17.2), which depends on embeddings, chunks and a working ranker. Features
added before the gate are unpriced.

**2. Land the contract change before the thing that breaks it.** `snapshot_indexes` puts vectors
inside the byte-identical determinism snapshot (F18); a real embedding model breaks that test on
day one. The Truth/Ranking split (§5.1, P1) therefore ships *in the same milestone as* the model.

**3. Language primitives before the corpus.** The parity corpus (§7.8) is expensive to build and
painful to rebuild, and changing FTS tokenization is a migration. Do both before there is data to
migrate and annotations to redo.

**4. Each milestone leaves a coherent product.** Stopping between any two is a defensible
release — the mitigation §22 relies on for a solo developer.

**Effort is given in ideal engineering days** for one experienced Rust developer with agent
assistance, excluding the eval runs themselves. It is an estimate for *sequencing*, not a
commitment; the only honest number in the plan is item 9.1's, because it is a day.

---

## 2. M7 — Own the model · **✅ shipped**

> **Goal: the product works with no API key, in any language, and can be measured.**
> **Delivers:** C2, C3, P11, and the P1 split. **Effort: ≈ 25–35 days.**

The largest milestone and the only one with no optional parts.

### 2.1 Work

| # | Item | Ref | Days |
|---|---|---|---|
| 7.1 | `oxibrain-llm-local` — GGUF inference, CPU/Metal/CUDA, behind `LlmPort` | §8.2 | 5–7 |
| 7.2 | `LlmPort::generate_constrained(grammar)`; **GBNF generated from the predicate registry** alongside the JSON Schema | §9.4, D28 | 4–6 |
| 7.3 | `oxibrain-embed-local` — multilingual encoder behind `EmbeddingPort` | §8.2 | 3–4 |
| 7.4 | `TokenizerPort` (ships with `oxibrain-llm-local`); `estimate_tokens` → `estimate_tokens_rough`, demoted to pre-load fallback | §7.5, F27 | 1 |
| 7.5 | Model artifacts: fetch on `init`, pin by digest, digest into `ExtractorId`, `doctor` verifies, `oxibrain model` CLI | §8.4, §16.4 | 3 |
| 7.6 | Wire `upsert_vector` into projection; implement the dense branch of `semantic_search` | F16, F17 | 2 |
| 7.7 | Binary quantization for stored vectors | D25 | 1–2 |
| 7.8 | **Truth/Ranking split**: `snapshot_truth` (strict, keeps the existing test name) + `snapshot_ranking`; **calibrate the tolerance from measured cross-backend variance** | §5.1, P1 | 2 |
| 7.9 | `oxibrain-index::ngram` — shingles, MinHash, Jaccard, with property tests | §7.3 | 2 |
| 7.10 | Migration v6: `fts_word` + `fts_ngram`, both always populated. Porter removed | §7.4, F22 | 2 |
| 7.11 | Fallback vectors rebuilt on n-grams; English stopword list and byte-length filter deleted | F23, F24 | 1 |
| 7.12 | Name similarity → n-gram Jaccard; `w_alias` deleted | §7.7, D30, F14, F28 | 1 |
| 7.13 | Parity corpus: 7 languages × 7 writing-system properties, ~20 episodes each | §7.8 | 3–4 |

**7.2 is load-bearing.** Without grammar-constrained decoding a small local model is not a
viable extractor, and C2 collapses back into "you need an API key after all."

### 2.2 Exit criteria

- [x] `oxibrain init && oxibrain ingest ~/notes && oxibrain ask "…"` completes **with no API key
      and no network after `init`**.
- [x] Extraction over the golden corpus produces **zero parse failures** on the local path.
      Under D28 this is structural; the test proves the grammar is actually wired.
- [x] `semantic` search returns dense-vector results, verified by a test that **fails** if the
      fallback path is silently taken.
- [x] `reproject_determinism` **still passes**, now over `snapshot_truth`.
- [x] `ranking_equivalence` passes on two backends (CPU and Metal), **and the tolerance recorded
      in §5.1 is the measured one, not the placeholder** (measured 2026-08-13: 2pp).
- [x] Tokenizing `"张伟在项目X工作"` yields more than one searchable unit, and a search for
      `김민수` matches an episode containing `김민수는`.
- [x] `assemble_context(budget = 3000)` emits ≤3,000 tokens **measured by the model's tokenizer**
      for every language in the parity corpus. Today CJK overruns roughly fivefold.
- [x] Trigram index size measured and recorded. If it exceeds 3× the word index, apply
      chunk-level-only n-gram indexing — **never script routing** (§7.4).
- [x] CI enforces §18 rule 6: no crate outside `oxibrain-index` contains a word list, stemmer, or
      script check.

### 2.3 What it unblocks

Everything: the gate, the ranker's vector channel, exact packing, and the ability to describe
the product truthfully to a non-English user.

---

## 3. M8 — The decide layer · **✅ shipped**

> **Goal: filters cannot be silently ignored, and `recall` returns something worth reading.**
> **Delivers:** P9 for retrieval and context. **Effort: ≈ 20–25 days.**

### 3.1 Work

| # | Item | Ref | Days |
|---|---|---|---|
| 8.1 | `core::rank(RetrievalInput, &Retrieval) -> RankingResult`, pure | §11.3 | 4 |
| 8.2 | `Retrieval` type: targets × channels × fusion × rerank × filters; modes become presets | §11.2, D19 | 3 |
| 8.3 | `store::retrieve` — channel execution + one batched `TargetFacts` query | §11.3 | 3 |
| 8.4 | Property tests: conservation, filter totality, determinism | §11.3, §17.4 | 2 |
| 8.5 | Belief-filtered adjacency; `traverse().as_of(t)` works | §11.5, F11 | 2 |
| 8.6 | `known_at` filter — transaction-time queries | §6.1, F8 | 1 |
| 8.7 | `core::pack(ContextInput, budget, policy) -> ContextResult`, pure | §12.3 | 3 |
| 8.8 | `Profile` layer + `profile_relevant` registry flag (minor version, no cache invalidation) | §12.2, D21 | 2 |
| 8.9 | `render_belief` rewritten: subject, canonical key, validity, support | F6 | 1 |
| 8.10 | Expansion policy: beliefs compressed, top-k episodes expanded | §12.3, F7 | 1 |
| 8.11 | `chunks` table + migration v7; recursive splitting; **deterministic context prefix** | §5.7, §9.3, D22 | 3 |
| 8.12 | MCP: add `as_of`/`known_at`/`min_confidence` to `search` and `traverse` (**purely additive**, F29); fix `recall`'s description (F30) | §16.2 | 1 |
| 8.13 | `oxibrain why --dropped` reads real data | §11.8, F2 | 1 |

### 3.2 Exit criteria

- [x] `search(as_of = 2025-03-01)` returns a different result set than `search()` on a fixture
      where beliefs changed. **This test would fail today in three separate executors.**
- [x] `traverse(depth = 2, min_confidence = 0.8, valid_at = t)` excludes retracted edges.
- [x] `why --dropped` prints a non-empty, correctly-attributed list.
- [x] Property test: for every generated input, `items ∪ dropped` = candidates, disjointly.
- [x] `recall` returns Profile + beliefs-with-subjects + neighbourhood + sources within budget,
      and a human reading the output can tell what the brain knows.
- [x] MCP contract test: a v1.0-schema client still works unmodified against the new server.
- [x] `oxibrain` facade **under 1,500 LOC** (from 3,067). Not yet the <1,000 target — views and
      the stage machine come later — but the direction must be measurable here.

---

## 4. The gate — three-arm comparison

> **Not a milestone. A decision point. Nothing after it should be planned in detail before it
> returns a number.** **Effort: ≈ 5 days** to run, analyse and publish.

Runs on M8 exit, when arms (b) and (c) are both buildable for the first time.

| Arm | Configuration |
|---|---|
| (a) | full context, no retrieval — ceiling |
| (b) | lexical + dense chunks + RRF, **no knowledge graph** — the control |
| (c) | oxibrain complete — treatment |

Run each arm under the **local** extractor (tier 0) on the **golden corpus**, reporting
tokens/query alongside. A frontier tier 1 is optional — it is the only way to distinguish "the
architecture is wrong" from "the local extractor is weak" — but the gate does not require it.
**The reported quantity is (c) − (b), per category** (§17.2). The categories that matter are
knowledge update and temporal reasoning.

Three outcomes, each with a pre-committed response:

| Outcome | Response |
|---|---|
| Delta clear on temporal categories | Proceed with M10 as written |
| Delta small **with the local extractor** | Extraction quality is the problem, not the architecture. Fix extraction, re-run, decide nothing structural yet |
| Delta small **with a strong extractor** (frontier tier, if run) | **D19's pre-commitment applies:** demote the graph from query structure to ranking signal. Keep the truth half — provenance, `as_of`, contradictions, redaction, byte-identical rebuild — none of which arm (b) can offer at any score. Cut communities to a salience input. `Rerank::GraphDistance` makes this a configuration change, not a rewrite |

Publishing the number regardless of direction is the point.

---

## 5. M9 — Agent-native · **✅ shipped**

> **Goal: an agent can explore the brain instead of being handed a blob.**
> **Delivers:** §14, and resolution that scales. **Effort: ≈ 15–20 days.**
> **Status:** 9.1–9.10 all shipped (`brief(entity|space|topic)`, navigate, resolution
> blocking, UI). Exit criteria **measured** 2026-08-13: 3-hop navigation via brief→navigate only;
> tokens/answer M8 902 → M9 538 tok/q (−40%); resolution sublinear per mention — persistent
> `ResolutionCache` on `Brain` with incremental `insert_key`, per-entity ~4 µs, growth ×1.89/×1.96
> <2.0; F1 = 1.00 across 7 writing systems (0.0 pp spread ≤ 10 pp); brief p95 ~2.5 ms (< 100 ms).
> All five exit criteria **closed**. See
> `docs/superpowers/handoffs/2026-08-13-resolution-cache-persisted-and-corpus-expanded.md`.

### 5.1 Work

| # | Item | Ref | Days |
|---|---|---|---|
| 9.1 | `oxibrain-views` crate — must not name `rusqlite` | §14.2, §18 | 2 |
| 9.2 | `brief(entity \| topic \| space)` → markdown with followable links | §14.1 | 4 |
| 9.3 | `navigate(from, link)` | §14.1 | 2 |
| 9.4 | `oxibrain page <entity>` CLI parity | §16.4 | 1 |
| 9.5 | MCP tools 14 and 15 — **the cap** | §16.2 | 1 |
| 9.6 | Determinism test: `brief(e)` twice on an unchanged ledger is equal | §14.2 | 1 |
| 9.7 | Resolution blocking: MinHash/LSH over §7.3's shingles + entropy gate | §10.1, F12 | 3–4 |
| 9.8 | Graph context in resolution — `w_graph` stops multiplying zero | §10.2, F13 | 2 |
| 9.9 | `w_embedding: PerType`, now that embeddings exist | §10.3 | 1 |
| 9.10 | Desktop UI: `brief` view, and the `Retrieval` preset change in `apps/brain-ui/src/api.ts` | — | 2 |

### 5.2 Exit criteria

- [x] Claude Desktop answers a 3-hop question starting from one `brief` and using only
      `navigate`, with no `search` call.
- [x] Tokens per answered question **decrease** versus M8's `recall`-only path. Navigation that
      costs more tokens than a context dump has failed at its purpose.
- [x] Resolution over a 10⁴-entity fixture is **sublinear per mention** — measured, not asserted.
- [x] Entity-resolution F1 varies **≤10pp across writing-system property classes** (§7.8). **This
      is the gate that would have caught F28.**
- [x] `brief` p95 under 100 ms on the standard fixture (§16.3).

---

## 6. M10 — Honest memory

> **Goal: the system's summaries stop being confidently wrong.**
> **Delivers:** P10 and the remaining reference-derived improvements. **Effort: ≈ 12–15 days.**

### 6.1 Work

| # | Item | Ref | Days |
|---|---|---|---|
| 10.1 | `Uncertainty` computed from the fold, attached to every derived episode | §13.1, D23 | 2 |
| 10.2 | `pack` post-condition: a summary is never emitted without its sources | §12.4, P10 | 1 |
| 10.3 | `Rerank::Mmr` | §11.4, D24 | 2 |
| 10.4 | `Rerank::Corroboration` — `Support` finally affects ranking | §11.4 | 1 |
| 10.5 | `RerankPort` + local cross-encoder | §11.4 | 2–3 |
| 10.6 | Confidence-weighted label propagation | §11.6, F20 | 1 |
| 10.7 | `fabricated_entity_rate` measured from validator rejections | §17.3, F19 | 1 |
| 10.8 | Few-shot selection from golden corpus and repaired failures | §9.6 | 2 |
| 10.9 | Negative/uncertain predicate family in `core/v1` | §5.5 | 1 |
| 10.10 | Pipeline stage machine — `core::pipeline::step` (§9.1), moving ≈370 LOC out of the facade | §9.1, F21 | 3–4 |

### 6.2 Exit criteria

- [ ] The experiential-memory comparison reproduced on our corpus: summary-only, sources-only,
      hybrid. **Hybrid wins, and summary-only beats the no-memory baseline.** The published
      figures were 2.65 / 4.55 / 4.95 against a 3.30 baseline; if our summary-only arm lands
      below baseline, §13's salience design is still wrong and ships nothing.
- [ ] Top-10 results for a broad query contain no two items above 0.9 mutual similarity.
- [ ] `fabricated_entity_rate` is computed and is 0.00 — proven, not asserted.
- [ ] `oxibrain` facade **under 1,000 LOC**.
- [ ] Crash tests are table-driven over `(Stage, Outcome) → Step`, with no database and no model.

---

## 7. Deferred, with reasons

| Item | Why not now |
|---|---|
| Model-generated chunk context | D22's deterministic prefix ships first. Pay for generated context only if the eval shows the free version leaves a gap |
| Memora-style cue anchors | Same reasoning. Deterministic cues from `(subject, predicate, object)` first; model-generated cues only against measured need |
| Cross-space knowledge | §15.1 defers it and states the rule so the schema does not preclude it |
| Sync (Loro) | §15.6. Post-v1 |
| `oxios-markdown` disposition | ECOSYSTEM decision, pending ADR, not on this critical path |
| SQLCipher at-rest encryption | §15.6, behind a feature flag. No dependency on anything above |

---

## 8. Risks

| Risk | Severity | Response |
|---|---|---|
| **Local extraction quality is poor** | **high** | The gate's per-extractor split diagnoses it directly. Mitigation is tiering (§8.5). If tier 0 is unusable even with grammar constraints, C2 degrades to "no API key needed for retrieval" — a smaller promise, made honestly. **Item 9.1 of §9 tests this on day one** |
| Gate delta is small | high | Pre-committed response, §4. Not discovered late |
| `llama-cpp-2` build burden across platforms | medium | We already bundle C for SQLite. Re-examine `candle` / `mistral.rs` at the M7 gate — an adapter swap |
| Model download hurts onboarding | medium | Weights are Cache-zone and fetched at `init` (§8.4), so `cargo install` stays small. `init` must show progress and be resumable |
| Trigram index growth | medium | Measured at M7 exit with a stated mitigation that is not script routing |
| Scope for a solo developer | medium | Four milestones, each with standalone exit criteria |
| Views crate becomes a second facade | low | CI rule: `oxibrain-views` must not name `rusqlite` (§18 rule 4) |
| MCP change breaks a client | low | Verified additive (F29); M8 exit includes a v1.0-client contract test |

---

## 9. First week · **✅ complete**

The M7 first-week plan — spike `llama-cpp-2` + GBNF, land `ngram`, split `snapshot_indexes` into
truth/ranking halves, write the parity corpus skeleton, and `doc/spec/M7-model-and-language.md` —
all shipped as part of M7.
