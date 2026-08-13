# Handoff — M10: the final milestone (finish everything → v1)

> **Read this first.** This is the handoff for the last milestone. M10 ships → v1.
> **Branch:** `main` · working tree clean · `cargo test --workspace` green (350/0/5) · `clippy -D warnings` clean.
> **Predecessor:** `2026-08-13-resolution-cache-persisted-and-corpus-expanded.md`.
> **ROADMAP:** §6 (M10), exit criteria in §6.2.

---

## The goal

M10 — "Honest memory" — makes the system's summaries stop being confidently wrong.
10 work items, ≈12–15 ideal days, 5 exit criteria. When all pass, oxibrain is v1.

---

## M10 item-by-item: current state → what to build

### 10.1 — Uncertainty from the fold → derived episodes (§13.1, D23) · ~2 days

**What exists:**
- `ConfidenceComponents` (`crates/oxibrain-core/src/confidence.rs`) is fully wired in the fold:
  `confidence = raw × calibrate(extractor) × corroboration × trust × recency`. All five components
  computed in `fold.rs::belief_confidence` (line 121).
- `SummaryWithUncertainty` (`pack.rs:81`) has `confidence: f32` + `sources: Vec<String>`.
- `Belief.confidence` is populated by the fold and queried everywhere.

**What's missing:**
- A formal `Uncertainty` struct attached to **derived episodes** (consolidation/community-summary
  episodes, `EpisodeKind::Derived`). P10: "compression may lose detail, never doubt." Every derived
  artifact must carry the uncertainty computed from its support (contradictions, single-source
  claims, staleness, trust exclusions).
- The uncertainty factors to compute per derived episode:
  - `contradiction_rate` = contradicted beliefs / total beliefs in the group
  - `single_source_fraction` = beliefs backed by only 1 episode / total
  - `staleness` = max age of supporting episodes relative to now
  - `trust_exclusion_fraction` = beliefs with untrusted support / total

**Build:**
1. `crates/oxibrain-core/src/uncertainty.rs` — `Uncertainty` struct + `compute(group, calibration)`
   pure function. Property-tested.
2. Attach to derived episodes: add `uncertainty_json TEXT` column to `episodes` (migration v8 with
   up-test from v7 fixture). Populate during consolidation (`store::consolidation`) and community
   summarization.
3. Surface in `pack` — summaries include their uncertainty in rendered output.

### 10.2 — pack post-condition: summary never without sources (§12.4, P10) · ~1 day

**What exists:**
- `SummaryWithUncertainty.sources: Vec<String>` is populated by consolidation and community summary.
- `pack()` includes summaries in the context output.

**What's missing:**
- A hard post-condition test: if `ContextResult` contains any summary, it must have non-empty
  `sources`. This is P10's executable form: "a summary is never returned without its sources."

**Build:**
1. Property test in `pack.rs` tests: for every generated `ContextInput`, if `summaries` is
   non-empty, every summary has `!sources.is_empty()` && `confidence > 0.0`.
2. Runtime assertion in `pack()`: if a summary with empty sources reaches pack, drop it and log.

### 10.3 — Rerank::Mmr with real embeddings (§11.4, D24) · ~2 days

**What exists:**
- `Rerank::Mmr { lambda: f32 }` variant in the enum (`rank.rs:116`).
- `apply_rerank` (`rank.rs:718`) implements it but uses a **proxy** similarity:
  `|fused_score_a - fused_score_b|` instead of real embedding cosine. Comment says "Without
  feature vectors we cannot compute the similarity term."

**What's missing:**
- Real embedding-based similarity. Now that `oxibrain-embed-local` exists and entities have dense
  vectors (§7.6), the rank function needs access to them. Currently `rank` is pure and has no I/O —
  embeddings must be supplied via `RetrievalInput` (pre-fetched by the store layer, same pattern as
  `TargetFacts`).
- Add `entity_vectors: HashMap<EntityId, Vec<f32>>` to `RetrievalInput`. Store layer populates it
  from `entity_embeddings` table for the candidate set.
- Rewrite `apply_rerank` MMR branch to use cosine similarity between entity vectors.

**Build:**
1. Add `entity_vectors` field to `RetrievalInput` (`rank.rs`).
2. Store layer: batch-fetch embeddings for the candidate entity set after rank produces initial
   items, before rerank. Same `TargetFacts` pattern.
3. Rewrite MMR to use cosine similarity. O(k²) is fine for k ≤ 50.

### 10.4 — Rerank::Corroboration (§11.4) · **essentially done** · verify ~0.5 day

**What exists:**
- `Rerank::Corroboration` is **already implemented** in `apply_rerank` (`rank.rs:696`):
  boosts multiplicatively by `1 + log(1 + distinct_episodes)`. Used as the default in
  `Retrieval::hybrid` preset (line 200).

**What to do:**
- Verify test coverage: property test that Corroboration never changes ordering when all
  `distinct_episodes` are equal (invariance), and that higher corroboration → higher rank.
- This is a checkbox, not a build.

### 10.5 — RerankPort + local cross-encoder (§11.4) · ~2–3 days

**What exists:**
- Nothing. `apply_rerank` is pure, synchronous, no I/O.

**What's missing:**
- `RerankPort` trait (in `oxibrain-ports`): `async fn rerank(query: &str, items: &[RerankItem])
  -> Result<Vec<RerankItem>>`. A cross-encoder scores (query, item_text) pairs.
- A local cross-encoder adapter (`oxibrain-rerank-local` or inside `oxibrain-llm-local`):
  uses the loaded GGUF model to score pairs. Could use the same model as extraction with a
  scoring prompt, or a dedicated small cross-encoder.
- `Rerank::CrossEncoder { port: Arc<dyn RerankPort> }` variant, applied after MMR/Corroboration.
  This is the only async reranker — applied in the store layer, not in pure `rank`.

**Build:**
1. Trait in `oxibrain-ports`.
2. Adapter in `oxibrain-llm-local` (scoring prompt: "Rate relevance 0-1: query=… doc=…").
3. Wire into `Retrieval` presets as an optional final stage.

### 10.6 — Confidence-weighted label propagation (§11.6, F20) · ~1 day

**What exists:**
- `label_propagation(graph: &AdjacencyGraph, max_iterations: usize) -> CommunityMap` in
  `crates/oxibrain-index/src/community.rs:9`. Unweighted — every edge has equal vote.
- `rebuild_communities` in `store::communities.rs` calls it with `10` iterations.

**What's missing:**
- Edge weights from belief confidence. An edge Alice→Bob with confidence 0.9 should carry more
  label-propagation weight than an edge with 0.3. This makes community detection respect evidence
  quality.

**Build:**
1. Add `label_propagation_weighted(graph: &WeightedAdjacencyGraph, max_iterations) -> CommunityMap`
   in `community.rs`. `WeightedAdjacencyGraph` edges carry `f64` weights.
2. `rebuild_communities` builds weighted edges from mean belief confidence per adjacency pair.

### 10.7 — fabricated_entity_rate measured (§17.3, F19) · ~1 day

**What exists:**
- Extraction validation (`extraction.rs::validate_claims`) rejects claims with unknown predicates,
  bad types, out-of-range confidence. Rejections go to `extraction_failures` table.
- `derive_calibration(precision, fabrication_rate)` in `confidence.rs:72` already takes a
  fabrication_rate parameter — but nothing computes it.

**What's missing:**
- A metric: of the entities created by extraction, what fraction were "fabricated" (the entity
  surface doesn't appear in the source text / was hallucinated by the model)?
- This is measured from the eval harness, not computed at runtime.

**Build:**
1. Eval harness step: after extraction, for each created entity, check if its surface appears in
   the source episode text. Report `fabricated_entity_rate = fabricated / total`.
2. Feed into `derive_calibration` to adjust per-extractor confidence.

### 10.8 — Few-shot selection from golden corpus (§9.6) · ~2 days

**What exists:**
- Golden corpus (25 episodes, 25 questions). Extraction grammar (GBNF) generated from registry.
- Extraction prompt includes system instructions but no few-shot examples.

**What's missing:**
- Few-shot examples in the extraction prompt: select 2–3 episodes from the golden corpus that are
  similar to the target episode, include their correct extraction output as examples.
- Selection mechanism: retrieve the most similar golden episodes by n-gram or embedding similarity
  to the target episode text.

**Build:**
1. `extraction::few_shot_examples(target_text, golden_corpus, k) -> Vec<Example>` — select k most
   similar golden episodes.
2. Inject into the extraction prompt before the target.
3. Eval: measure extraction quality delta with vs without few-shot.

### 10.9 — Negative/uncertain predicate family in core/v1 (§5.5) · ~1 day

**What exists:**
- 12 predicates in `CORE_V1` (`registry.rs:127`). Polarity field on assertions (`Affirm`/`Deny`).

**What's missing:**
- Predicates for uncertainty: e.g. `allegedly_employed_by`, `rumored_knows`. These let the system
  represent hearsay without polluting the belief fold with low-confidence assertions that look like
  facts.
- Or: a registry-level `uncertainty_marker` flag on predicates that marks assertions as uncertain
  by default.

**Build:**
1. Add 2–3 predicates to `CORE_V1` with appropriate semantics.
2. Update extraction grammar to include them.
3. Test: extraction produces them, fold treats them as low-confidence by default.

### 10.10 — Pipeline stage machine (§9.1, F21) · ~3–4 days · **foundational**

**What exists:**
- The facade (`crates/oxibrain/src/lib.rs`) is **1694 LOC, 56 methods**. The extraction pipeline
  (ingest → extract → validate → project → fold → index) is inline across multiple methods:
  `ingest`, `extract_one`, `extract_one_with`, `extract_pending`, `reextract`, `consolidate`,
  `summarize_communities`. Each duplicates the spawn_blocking + writer.submit + channel pattern.
- M8 exit criterion was "facade under 1500 LOC" (met). M10 exit criterion: **under 1000 LOC**.

**What's missing:**
- `core::pipeline::step(stage, input) -> StepResult` — a pure state machine for the extraction
  pipeline. Each stage (Ingest, Extract, Validate, Project, Fold, Index) is an enum variant. The
  step function transitions between stages. The facade calls step() instead of inlining the logic.
- Crash tests become table-driven: `(Stage, Outcome) → Step` with no database and no model.

**Build:**
1. `crates/oxibrain-core/src/pipeline.rs` — `Stage` enum, `PipelineInput`, `StepResult`,
   `fn step(stage, input) -> StepResult`. Pure, no I/O.
2. Stages:
   - `Ingest` → reads episode text, produces `Episode`
   - `Extract` → calls LLM (via port), produces `ExtractionResponse`
   - `Validate` → calls `validate_claims`, produces `ValidationResult`
   - `Project` → calls `project_extraction` (store), produces projection summary
   - `Fold` → re-folds affected groups
   - `Index` → FTS + embedding index update
3. Facade methods become thin wrappers: call step() inside spawn_blocking, handle I/O.
4. Target: move ~700 LOC out of the facade into pipeline + store.

---

## M10 exit criteria (ROADMAP §6.2)

- [ ] The experiential-memory comparison reproduced on our corpus: summary-only, sources-only,
      hybrid. **Hybrid wins, and summary-only beats the no-memory baseline.** Published figures
      were 2.65 / 4.55 / 4.95 against a 3.30 baseline; if summary-only lands below baseline,
      §13's salience design is wrong and ships nothing.
- [ ] Top-10 results for a broad query contain no two items above 0.9 mutual similarity.
- [ ] `fabricated_entity_rate` is computed and is 0.00 — proven, not asserted.
- [ ] `oxibrain` facade **under 1,000 LOC**.
- [ ] Crash tests are table-driven over `(Stage, Outcome) → Step`, with no database and no model.

---

## Suggested execution order (dependency-aware)

**Wave 1 — Foundation (do first, unblocks everything):**
1. **10.10** Pipeline stage machine — reduces facade LOC (exit criterion), makes 10.1/10.2/10.7
   cleaner. Biggest item (~3–4 days) but highest leverage.
2. **10.9** Negative/uncertain predicates — independent, small, no dependencies.

**Wave 2 — Core quality (after pipeline):**
3. **10.1** Uncertainty from fold — builds on existing confidence, needs migration v8.
4. **10.2** pack post-condition — test only, depends on 10.1.
5. **10.4** Corroboration verify — checkbox, already implemented.

**Wave 3 — Retrieval quality:**
6. **10.3** Mmr with real embeddings — needs entity_vectors in RetrievalInput.
7. **10.6** Confidence-weighted label propagation — independent.
8. **10.5** RerankPort + cross-encoder — depends on 10.3 being real. Biggest uncertainty (model
   quality for scoring).

**Wave 4 — Eval & extraction:**
9. **10.7** fabricated_entity_rate — eval harness addition.
10. **10.8** Few-shot selection — depends on golden corpus + extraction.

---

## Gate decision context (ROADMAP §4)

The gate was run on the expanded corpus (25 episodes / 25 questions / 32 declarations):
- **delta(c−b) = 0** for both categories at this scale.
- **Tokens/answer: M8 902 → M9 538 (−40%)** — brief path is cheaper regardless.

ROADMAP §4 pre-committed outcomes:
- "Delta clear on temporal" → proceed M10 as written. **Not met** (delta = 0).
- "Delta small with local extractor" → fix extraction. **Applicable but extraction isn't in the
  gate** (declarations are direct).
- "Delta small with strong extractor" → D19's demote. **This is the expected path**: graph →
  ranking signal (`Rerank::GraphDistance`), not primary retrieval. The truth half (provenance,
  `as_of`, contradictions, redaction, byte-identical rebuild) stands regardless.

**Practical guidance:** M10's items (MMR, Corroboration, cross-encoder, uncertainty) improve
quality regardless of the gate outcome. Build them. The gate decision (D19 demote vs proceed)
can be made after M10 when the corpus is larger.

---

## Key files and locations

| What | Where |
|---|---|
| Facade (1694 LOC, target <1000) | `crates/oxibrain/src/lib.rs` |
| Confidence formula | `crates/oxibrain-core/src/confidence.rs` (141 LOC) |
| Fold (belief_confidence) | `crates/oxibrain-core/src/fold.rs:121` |
| Rank + Rerank enum + apply_rerank | `crates/oxibrain-core/src/rank.rs` (1104 LOC) |
| Pack (context assembly) | `crates/oxibrain-core/src/pack.rs` (677 LOC) |
| Extraction + validation | `crates/oxibrain-core/src/extraction.rs` (936 LOC) |
| Registry (CORE_V1, 12 predicates) | `crates/oxibrain-core/src/registry.rs:127` |
| Label propagation | `crates/oxibrain-index/src/community.rs:9` |
| Communities (store) | `crates/oxibrain-store/src/communities.rs` |
| Consolidation | `crates/oxibrain-store/src/consolidation.rs` |
| Gate runner | `crates/oxibrain-cli/src/cmd/gate.rs` |
| Golden corpus | `eval/golden/` (25 episodes, 25 questions) |

## Verification commands

```bash
cargo test --workspace                    # all tests
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo build -p oxibrain --no-default-features --features http-llm  # standalone
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1       # no oxi crates
cargo run -p oxibrain-cli -- eval --suite gate                      # gate runner
wc -l crates/oxibrain/src/lib.rs                                    # facade LOC (<1000)
```

## Architecture invariants (still in force)

- P1: Ledger immutable, projection rebuildable. Truth half byte-identical.
- P9: Store fetches/writes, core decides, facade sequences.
- P10: Compression may lose detail, never doubt. ← **This is M10's headline.**
- Fifteen MCP tools is the cap.
- No `rusqlite` in `oxibrain-core` or `oxibrain-views`.
- No `rank`/`pack`/`step` in `oxibrain-store`.
- Migration v8 (if needed for uncertainty) requires up-test from v7 fixture.
