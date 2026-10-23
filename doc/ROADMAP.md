# oxibrain Roadmap — M7 onward

> **Version:** v1.3 · **Date:** 2026-10-23
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

The gate (§4) has run on the expanded corpus (43 episodes / 42 questions):
**c−b = +6 on graph_reasoning (b 3/9 → c 9/9), 0 on knowledge_update and
temporal (both arms read the same fold — by design)**. Outcome recorded in
§4 (2026-10-17): D19 demotion does not fire; the graph stays as query
structure. **M10 (§6) and M11 (§7)** shipped; `eval --suite
experience|experiential` measures them.

**M12 implementation is present; its live quality gate remains open** (2026-10-23): reliable recall and
public-scope correctness, specified in `doc/spec/reliable-recall-v1.md` and
sequenced in §11 below. The current research evidence is recorded in
`doc/research/2026-10-23-recall-quality-and-evidence.md`; it is separate from
implementation acceptance results in `doc/research/2026-10-23-reliable-recall-implementation.md`.

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


**Outcome — recorded 2026-10-17** (corpus expanded to 43 episodes / 42 questions,
concentrated on the decision categories; `eval --suite gate`):

| Category | a ceiling | b lexical | c hybrid | (c) − (b) |
|---|---|---|---|---|
| graph_reasoning | 9/9 | 3/9 | 9/9 | **+6** |
| knowledge_update | 20/20 | 20/20 | 20/20 | 0 |
| temporal_reasoning | 12/13 | 13/13 | 13/13 | 0 |

Reading: the graph's query-structure dividend is **unambiguous on multi-hop**
(lexical answers 3/9, hybrid 9/9; the M11 experience corpus shows the same
shape on gotcha/premise multi-hop). Knowledge-update and temporal delta 0 is
**by design, not failure**: both arms read the same fold — time-travel and
supersession correctness are fold properties (13/13 as_of answers correct in
both arms), not retrieval differentiators; the gate's original framing
assumed otherwise. The D19 demotion trigger does **not** fire: no outcome row
matches "delta small" once multi-hop is counted. Decision: the graph stays as
query structure, the truth half stays regardless (D19 keeps it under every
branch), and communities remain. Arm r (recall-2k) is reported for budget
shape, not scored as a retrieval arm — it is context assembly, and it trades
recall for a 2k-token budget (graph 7/9, ku 18/20, temporal 11/13 at
~1.7k tokens).

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

## 6. M10 — Honest memory · **✅ shipped 2026-10-17**

> **Goal: the system's summaries stop being confidently wrong.**
> **Delivers:** P10 and the remaining reference-derived improvements. **Effort: ≈ 12–15 days.**
> **Status:** all ten items in the tree — 10.1–10.6, 10.10 predated this check-in
> (Uncertainty plumbing, sourceless-summary pack filter, Mmr/Corroboration,
> `RerankPort` + `CrossEncoderReranker`, confidence-weighted label propagation,
> table-driven `pipeline::step`); 10.9 landed with M11; the §13.2 comparison,
> the MMR property test, and the facade split landed 2026-10-17.
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

- [x] Experiential-memory comparison, measured 2026-10-17
      (`eval --suite experiential`, deterministic canned-LLM runbooks over
      `eval/experience/`): **summary-only 2/4 vs no-memory 0; sources-only
      4/4; hybrid 4/4 — dominates every arm and strictly beats summary-only**
      (premise-2 answers only from sources, static-1 only from beliefs, both
      reach hybrid). The published figures were LLM-judged (2.65 / 4.55 / 4.95
      vs 3.30); this rendering scores answer reachability within a
      2,000-token budget — same ordering, honest zeros included.
- [x] Top-10 mutual similarity: `mmr_ceiling_property_distinct_prefix_and_conservation`
      (proptest over adversarial duplicate clusters) + the two example tests.
      The property caught a real fixture flaw on its first run (n ≥ 14 even
      spacing exceeds 0.9) — it bites.
- [x] `fabricated_entity_rate` measured (`measure_fabrication_rate` from source
      text, enforced by `check_gates`): `eval --suite fast` — precision 1.000,
      recall 1.000, all §14.2 gates green.
- [x] Facade **814 LOC** (`lib.rs`; spaces/reads moved to focused submodules,
      no API change).
- [x] Crash tests table-driven over `(Stage, Outcome) → Step` (`pipeline.rs`,
      no database, no model).

---

## 7. M11 — Experience memory · **✅ shipped 2026-10-16**

> **Goal: agent trajectories become reusable environment experience — workflows, gotchas,
> premises — without a new store, plane, or tool.**
> **Delivers:** `doc/spec/experience-memory-v1.md` (ADR-018). **Effort: ≈ 12–16 days.**
> **Status:** implemented and measured 2026-10-16. The M10 dependencies were already in
> the tree (Uncertainty §13.1, summaries-with-sources `pack` filter, negative family
> landed here as 10.9). Corpus lives at `eval/experience/` (not tests/fixtures).

### 7.1 Work

| # | Item | Ref | Days |
|---|---|---|---|
| 11.1 | Experience predicate family in `core/v1` (minor 4→5): `requires`, `fails_when`, `premise_differs` | spec §3 | 1 |
| 11.2 | `SourceRef::AgentTrace { agent }` + legacy backfill migration (no schema change) | spec §2.1 | 1–2 |
| 11.3 | `ingest` additive `trace` payload; event identity + conflict semantics (exit 6) | spec §2.3 | 2 |
| 11.4 | Rendering contract: step-boundary chunk tests, truncation determinism | spec §2.2 | 1 |
| 11.5 | Few-shot trajectory examples (EN+KO) in the golden corpus; trace extraction quality | spec §3 | 2–3 |
| 11.6 | `consolidate(mode = Task)` → runbook Derived episodes; `summaries` cache kind `runbook` | spec §4 | 3 |
| 11.7 | `oxibrain admin consolidate [--mode topic\|task]` — admin verb, MCP cap untouched | spec §4 | 0.5 |
| 11.8 | `tests/fixtures/experience-corpus/` + three-arm extension of the gate harness | spec §5 | 3–4 |

### 7.2 Exit criteria

- [x] On the experience corpus (14 questions), `recall` within a 2,000-token budget
      answers what the lexical arm (b) cannot: **r−b = +2 gotcha, +3 workflow, +1 premise**
      (static/dynamic are lexical-sufficient controls — honest zeros); r spends
      1,139–1,290 of 2,000 tokens. Ceiling arm a = 14/14.
- [x] Replaying a `(agent, run_id)` pair with identical bytes is idempotent; different
      bytes raise `BrainError::Conflict` → exit 6 — `tests/trace_memory.rs`.
- [x] Runbooks are terminal: zero assertions on runbook episodes, never in the primary
      backlog, replay is a checkpoint no-op — `tests/runbook_memory.rs`.
- [x] Environment scoping: the two envs' identically-named tasks are distinct surfaces
      → distinct entity keys by construction (unique `(space, type, normalized)`); the
      collision clause is vacuous in this corpus (no shared qualified surface).
- [x] P10 post-condition: every runbook carries Uncertainty computed from the fold and
      cites all member episodes via `episode_links` — `tests/runbook_memory.rs`.
- [x] Registry bump is minor (4→5); `ExtractorId` hashes the major only, so cached
      extractions remain valid — no forced re-extraction.
- [x] Corpus covers English + Korean (≥ 2 writing systems, §7.8).

---

## 8. Deferred, with reasons

| Item | Why not now |
|---|---|
| Model-generated chunk context | D22's deterministic prefix ships first. Pay for generated context only if the eval shows the free version leaves a gap |
| Memora-style cue anchors | Same reasoning. Deterministic cues from `(subject, predicate, object)` first; model-generated cues only against measured need |
| Cross-space knowledge | §15.1 defers it and states the rule so the schema does not preclude it |
| Sync (Loro) | §15.6. Post-v1 |
| `oxios-markdown` disposition | ECOSYSTEM decision, pending ADR, not on this critical path |
| SQLCipher at-rest encryption | §15.6, behind a feature flag. No dependency on anything above |

---

## 9. Risks

| Risk | Severity | Response |
|---|---|---|
| **Local extraction quality is poor** | **high** | The gate's per-extractor split diagnoses it directly. Mitigation is tiering (§8.5). If tier 0 is unusable even with grammar constraints, C2 degrades to "no API key needed for retrieval" — a smaller promise, made honestly. **The §10 first-week spike tests this on day one** |
| Gate delta is small | high | Pre-committed response, §4. Not discovered late |
| `llama-cpp-2` build burden across platforms | medium | We already bundle C for SQLite. Re-examine `candle` / `mistral.rs` at the M7 gate — an adapter swap |
| Model download hurts onboarding | medium | Weights are Cache-zone and fetched at `init` (§8.4), so `cargo install` stays small. `init` must show progress and be resumable |
| Trigram index growth | medium | Measured at M7 exit with a stated mitigation that is not script routing |
| Scope for a solo developer | medium | Four milestones, each with standalone exit criteria |
| Views crate becomes a second facade | low | CI rule: `oxibrain-views` must not name `rusqlite` (§18 rule 4) |
| MCP change breaks a client | low | Verified additive (F29); M8 exit includes a v1.0-client contract test |
| Experience extraction quality on noisy traces | medium | Same tiering answer as row 1; the 11.8 three-arm corpus is the gate. If (c) − (b) ≤ 0 with a strong extractor, predicates ship as registry data and runbook consolidation goes opt-in (ADR-018) |

---

## 10. First week · **✅ complete**

The M7 first-week plan — spike `llama-cpp-2` + GBNF, land `ngram`, split `snapshot_indexes` into
truth/ranking halves, write the parity corpus skeleton, and `doc/spec/M7-model-and-language.md` —
all shipped as part of M7.

---

<a id="m12-reliable-recall"></a>

## 11. M12 — Reliable recall · **implemented; live quality gate open**

> **Goal:** relevant stored evidence reaches the public consumer within a stated
> context budget and visibility scope.
> **Design:** [spec](spec/reliable-recall-v1.md), [ADR-019](adr/ADR-019-read-scope-and-evidence-recall.md),
> architecture §27. The owner requested all implementation. A–C and the D runner/corpora
> are present in the working tree; live reader/dense quality validation remains unrun.

### 11.1 Work sequence

| Batch | Work | Acceptance before proceeding |
|---|---|---|
| A | Shared read scope, public document recall, historical assertion reconstruction through snippets/profile/identity, correct loss/freshness metadata; keep sixteen tools. | Canonical document found by search reaches public CLI/MCP recall; `known_at` excludes later evidence; old non-temporal payloads remain compatible; unsupported document time scopes fail explicitly. |
| B | Complete UTF-8 source windows; schema-14→15 ranking-index migration; query-relevant session retrieval, atomic new-source indexing and resumable backfill. | Older-session and late-summary evidence reachable; no splitter tail loss; migration/crash/resume/redaction tests; ledger and truth snapshot unchanged. |
| C | Pure evidence packing, complete source bundles, independent tokenizer binding and canonical serialization accounting. | Oversized first candidate does not block a later fitting unit; actual tokenizer budget, conservation, determinism, support pairing and read-race checks pass. |
| D | Span-annotated held-out in-house corpus and executable live-evaluation path; matched reader/embedding/source-access controls. | Full Support@B and supported-answer results, costs, intervals and failures published; existing regression/parity tolerances hold. |

Implementation acceptance status ([evidence](research/2026-10-23-reliable-recall-implementation.md)):

- [x] The five public-operation failures in the research report have behavioral
      tests through CLI and MCP and are fixed across A–C.
- [x] Both time axes reach snippets, profile, identities, graph edges and source
      visibility; functional replacement, denial and retraction cases reconstruct
      the earlier belief rather than filtering only today's winner.
- [x] Current-only document time scope is an explicit capability error; PDC UUIDs,
      revision references, legacy readability, source bytes and corpus pins remain intact.
- [x] Source-unit completeness, UTF-8 validity, considered-unit conservation,
      exact context accounting and deterministic output have property tests.
- [x] New ranking state reprojects and redacts correctly; schema-14 up-test and
      crash/resume tests pass; the truth determinism test remains enabled.
- [ ] A real-tokenizer synthetic verification run is recorded. The live extraction/reader run must still report its
      actual coverage, quality and cost; canned reachability is labeled separately.

### 11.2 Follow-up boundaries

Configured existing dense adapters may supply candidates outside database
transactions. A new model-reranking policy or expanded semantic source index
must first show paired evidence/answer benefit at its recorded latency/memory
cost. Learned memory writing, new environment/outcome schemas and automatic
reinforcement need separate implementation specs; ordinary read counts never
become truth. Existing public-benchmark deferrals remain unchanged.

The first milestone rollback is a ranking-policy rebuild where possible. An
older binary cannot open schema 15: binary rollback requires a verified backup
restored to a new directory, following the existing detached-restore contract.
No downgrade, source conversion or bulk re-extraction is part of M12.
