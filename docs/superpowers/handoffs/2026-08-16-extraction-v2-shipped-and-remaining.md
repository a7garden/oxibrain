# Extraction contract v2 shipped — remaining work

> **Date:** 2026-08-16 · **Commits:** `f8b4115` (feat) · `d4d997c` (docs) · base `ba8f120`
> **State:** working tree clean on `main`. All gates green.
> **Refs:** `doc/adr/ADR-006-quote-based-mention-evidence.md` ·
> `docs/superpowers/specs/2026-08-16-quote-based-mention-evidence-design.md` ·
> ARCHITECTURE.md v2.4 §9.4 · ROADMAP.md §6 (M10) / §4 (the gate)

## 1. What shipped this session

**Quote-based mention evidence (extraction contract v2).** The model no longer emits numeric
spans — measured span hallucination on qwen2.5-1.5b quarantined 4/4 claims on real notes.
Each mention / literal value now carries a verbatim `quote`; the server locates it (first
occurrence, exact bytes) and derives the byte span. The injection suite passes unchanged
(relocation of model spans stays forbidden), plus 2 new tests (fabricated-quote rejection,
code-block-quote-is-data). Legacy span-format responses keep the old ladder (eval fast
proves it). `prompt_version` 1 → 2 (CLI, MCP sampling, eval) — caches never mix contracts.
Few-shot selection (10.8) is actually wired now, with a built-in multilingual corpus whose
every example is validated by test. A multibyte panic in the casing fallback was found,
reproduced, fixed (`get(..n)` skip), and regression-tested.

**Measured effect (live, Metal, fresh stores):** Korean note 0/2 → **1/2** claims extracted
(residual: particle paraphrase 는→가, correctly rejected). English note stays **0/2**
(field-role confusion: correct quote copied into the wrong field — model-tier boundary, not
a contract defect; quarantine works, `beliefs` never contaminated).

**Gates at ship time:** 433 tests / 0 failed · clippy `-D warnings` clean · fmt · injection
suite 7/7 · `eval fast` all quality gates (fabricated 0.000, P 1.000, R 1.000) · standalone
guarantee (no oxi crates) holds.

## 2. Remaining work, in priority order

### P1 — Local extraction quality on freeform notes (production blocker for "no API key")

EN note yields 0 valid claims; KO yields 1/2. The contract is fixed; the residual is the
1.5B model. Three options, cheapest first:

1. **7B local model** — `oxibrain model pull` a qwen2.5-7b-instruct GGUF (q4_k_m ~4.7 GB),
   `oxibrain model use`, re-measure the two smoke notes. Model swap is a config event
   (§9.5: digest changes ExtractorId; old caches preserved). Watch MTL memory (~4–6 GB).
2. **HTTP tier for extraction only** — existing `oxibrain-llm-http` path; C2 degrades
   honestly to "no API key needed for retrieval" per ROADMAP §8 risk row 1.
3. **Prompt iteration** — diminishing returns measured: two iterations (SAME-sentence
   instruction, nonempty-quote grammar forcing) moved KO 0→1, EN 0→0. Greedy decoding
   reproduces outputs byte-identically; only structural prompt changes matter.

**Acceptance:** ≥1 valid statement per note on both KO and EN smoke notes, eval fast still
green, injection suite green. Smoke recipe: §4 commands — always a FRESH store
(`OXIBRAIN_DIR=$(mktemp -d)/brain`): the extraction cache is keyed `(episode_id,
extractor_id)` and will replay the old bad response for an unchanged extractor id.

**Known failure signatures** (from extraction_failures):
- `surface_not_verbatim` + quote is a paraphrase (은/를 swapped, 하다/한다) — validator
  correct, do not weaken.
- Subject quote = wrong sentence entirely (model anchors on the note's first line) —
  field-role confusion; try few-shot corpus entries shaped like date-prefixed decision
  notes if iterating prompts.
- Object emitted as `literal` where registry wants Entity (`works_on` → Project) — few-shot
  examples already model the entity form; a 7B model resolves this reliably.

### P2 — M10 exit criteria (implementation mostly landed; verification outstanding)

ROADMAP §6 checkboxes are all unchecked but the tree disagrees — verify and tick what is
measurably done:

| Item | In tree | Exit evidence missing |
|---|---|---|
| 10.1 Uncertainty | `crates/oxibrain-core/src/uncertainty.rs`, wired to consolidation + views | — (verify wiring covers every derived episode) |
| 10.2 pack sources | `pack.rs` drops empty-source summaries | — |
| 10.3/10.4 Mmr, Corroboration | `rank.rs` + tests | **top-10 ≤0.9 mutual-similarity check unmeasured** |
| 10.5 RerankPort | `CrossEncoderReranker` in `oxibrain-llm-local` | — |
| 10.6 label propagation | confidence-weighted (verify) | — |
| 10.7 fabricated rate | computed in eval | — (already 0.000 measured) |
| 10.8 few-shot | wired this session | — |
| 10.9 hearsay predicates | `allegedly_employed_by`, `rumored_knows` in registry | — |
| 10.10 pipeline::step | table-driven, no DB/model (`pipeline.rs`) | — |

**Real remaining M10 work:**
1. **Experiential-memory comparison** (the exit gate): summary-only vs sources-only vs
   hybrid on our corpus; published reference 2.65/4.55/4.95 vs 3.30 baseline. Needs eval
   harness arms in `crates/oxibrain-cli/src/cmd/eval.rs` (currently extraction-only).
2. **Facade <1,000 LOC**: `crates/oxibrain/src` totals **2,524** (lib.rs ~1,105). Next
   extraction targets: `models.rs` (330), `render.rs` (151), `pull_plan.rs` (121) — move
   behind views/store per P6/P9.
3. MMR mutual-similarity measurement over a real query set.

### P3 — The gate (three-arm comparison, ROADMAP §4)

Blocks nothing structural anymore but decides the graph's final role (D19 pre-commitment).
Requires the ~200-episode / ~100-question corpus (`eval/golden/manifest.toml` already
defines categories; 25/25/32 exist). Largest single effort left; consider deferring behind
P1/P2 until extraction quality is tier-stable, otherwise the gate prices a weak extractor.

### P4 — Filed follow-ups (brain-ui ledger, non-spec, from `.superpowers/sdd/brain-ui-v2/progress.md`)

Search loser-id resolution on merged surfaces · FK-after-failed-declare (ResolutionCache
lead — recurring, unexplained) · EntityPage loser header surface · offline latency ·
empty search snippets for entity-surface hits · T8 canvas z-order on empty transition ·
deterministic surface tie-break (T3 minor). Also ROADMAP §7 deferred: Loro sync, SQLCipher,
cross-space, model-generated chunk context, cue anchors.

## 3. Code anchors (read before touching extraction)

- `crates/oxibrain-core/src/extraction.rs` — `MentionRef`/`ClaimObject` (quote fields),
  `resolve_mention` ladder (step 0 = quote-locate), `locate_in_quote` (pure; ASCII-case
  fallback skips mid-char offsets), `grammar_from_registry` (nonempty-string rule),
  `schema_from_registry`, `build_extraction_prompt` (v2), `default_few_shot_corpus`
  (validated by `few_shot_corpus_examples_validate` — extend examples there, never inline).
- `crates/oxibrain/src/extraction.rs` — `extract_one_with_impl`: prompt composition
  (base + few-shot, k=2), grammar-vs-schema branch, repair loop (1 attempt,
  quote-aware message), projection + quarantine writes.
- `crates/oxibrain-store/tests/injection_suite.rs` — the contract's security edge; any
  change to `resolve_mention`/`locate_in_quote` must keep all 7 green.
- `crates/oxibrain-llm-local/src/lib.rs` — ChatML formatting, greedy decoding (deterministic
  outputs), `generate_blocking`.

## 4. Commands

```bash
# Gates
cargo test --workspace && cargo clippy --all-targets --all-features -- -D warnings \
  && cargo fmt --all -- --check && cargo run -q -p oxibrain-cli -- eval --suite fast
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1

# Live extraction smoke (ALWAYS a fresh store — cache replays otherwise)
export OXIBRAIN_DIR=$(mktemp -d)/brain
cargo run -q -p oxibrain-cli -- init
cargo run -q -p oxibrain-cli -- ingest <note.md>
cargo run -q -p oxibrain-cli -- extract <episode-id>     # ~45–95 s/episode on M4
sqlite3 "$OXIBRAIN_DIR/brain.db" "SELECT episode_id, errors_json FROM extraction_failures;"

# Model tier swap (P1 option 1)
cargo run -q -p oxibrain-cli -- model pull && cargo run -q -p oxibrain-cli -- model use
```

## 5. Risks & gotchas learned this session

- **Extraction cache**: `(episode_id, extractor_id)` key. Any prompt/grammar change without
  a `prompt_version` bump silently replays stale responses in existing stores. Bump on
  every contract-touching change.
- **Anchored edits drifted three times** on `extraction.rs` (~1,650 lines) — always re-read
  the exact region before `PUT`; format! templates there use inline `{var}` capture (no
  positional args).
- **Advisory review caught two real defects** my green tests missed: the "single-occurrence
  relocation" variant would break `verbatim_surface_required` (Alice occurs once), and the
  multibyte panic lived exactly where no ASCII-only test looked. Keep both patterns in any
  future validator work: check the security suite's letter, and test fallback paths on
  multibyte windows.
- **eval fast fixtures are legacy-format** (spans, no quotes) — intentional; they pin the
  compat ladder. New fixture formats should use quotes.
- Determinism: greedy decoding means identical prompts → identical failures. Re-measures
  after prompt-only changes need byte-diffing the response to confirm the change shipped.
