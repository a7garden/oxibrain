# Post-M12 remaining implementation and measurements

Date: 2026-10-24 (Asia/Seoul). Working-tree implementation following the owner's
request to implement the remaining proposals in
[the study](2026-10-23-post-m12-improvements.md).

The remaining source-recall, channel-receipt, complete-group-selection, packing,
and reader-evaluation code is implemented. The live experiment has run; **the
broad answer-quality gate remains open**. Complete source delivery and a valid
JSON response do not establish supported, appropriately conditional answers.

## Implementation

- Optional dense document recall and primary/session source-window retrieval.
  Schema 16 stores rebuildable memory vectors bound to source revision, window
  policy, verified model identity and dimension. Explicit
  `admin index --memory --embed` indexes them without extraction. Reads embed the
  query, retain independent lexical candidates, and report missing coverage or
  adapter failure. Eligibility precedes dense limits; the bounded scan reports
  exhaustion. Changed model weights invalidate reuse. Anonymous adapters have a
  runtime-only cache identity.
- Per-plane/channel eligibility, execution, coverage, model binding, considered
  count, scan caps, return-limit saturation and fallback receipts. A configured
  adapter is distinct from a completed semantic index. The lexical control is
  `ReadPolicy.semantic_sources=false`.
- Query-relevant selection of **complete** subject/predicate assertion groups
  before materialization. The core selects using bounded historical source seed
  metadata, then folds all competing values, denials and retractions at K.
  Historical identity inputs and live support are fetched for selected groups
  and their visible merge closure. No cached belief or ranking score becomes
  fold input. The metadata scan remains linear; no truth cache was introduced.
- Accepted canonical renders/counts and unchanged layer token counts are reused.
  Multi-layer context is still counted as a whole string. A frozen reference
  verifies identical context, source coordinates, decisions and exact tokens.
  Citation syntax is preserved; compact citation syntax was an optional research
  direction and was not needed for this optimization.
- `admin eval --suite reader` runs an explicitly configured reader without the
  full extraction suite. Its four arms are retrieved, legacy, complete gold, and
  no evidence. Frozen cases cover conflict, correction, conditions, absent
  answers, unspecified environment, versions, renamed entities, multiple sources,
  time and identifiers. Captures retain requests/responses for review. Action
  labels, exact answers, required phrases and citation containment are separate
  diagnostics.

The initial live run exposed two process-global llama.cpp initializers: loading
both local adapters panicked with `BackendAlreadyInitialized`. A shared
`oxibrain-llama-runtime` crate now owns initialization; repeated/concurrent access
is tested and an external initialization conflict returns a typed error. The
successful experiment used both local models together.

Tokio was updated from 1.53.1 to 1.53.3 to resolve the advisory found in the first
implementation's verification. No advisory exemption was added. Schema-16
creation can resume after a partial migration; a full-workspace test caught a
replayed migration failure before this was corrected.

Architecture v2.26, [ADR-020](../adr/ADR-020-source-candidates-and-truth-selection.md)
and [the implementation specification](../spec/post-m12-source-recall-v1.md)
record the adopted behavior. The earlier heading expansion and scope-before-top-k
changes remain covered by [their report](2026-10-24-post-m12-retrieval-fixes.md).

## Real-model experiments

Both corpora and their hashes were frozen before running. These are authored
synthetic diagnostics, **not independent human annotations, an external benchmark,
or a representative user sample**. No prompt/ranking changes were tuned against
the results. The evaluated release binary's SHA-256 was
`b743e1e523901576ece93bb5d5e46488a93878ff7521e0a9a86bd2fba61ccdc4`.
Later changes add migration retry protection, help text, source-count receipts and regression coverage;
they do not change this experiment's retrieval policy or prompt.

The existing local BGE-M3 Q4_K_M embedder (1024 dimensions, context 8192) and
Qwen3-4B-Instruct-2507 Q4_K_M reader were used. Exact context counts use the
independently pinned Qwen tokenizer. Full artifact digests and fixed prompts are
in the reports. No model download, paid provider or production store was used.

### Source coverage controls

[Five paired source occurrences](../../eval/reliable-recall/semantic-controls.jsonl)
span documents, sessions and primary episodes. Each occurrence has a synonym
or cross-language question and an exact-key question; reused keys refer to the
same occurrence, not independently ingested equal bytes.

At **both** 2,000 and 3,000 exact tokens:

| Questions | Optional semantic + lexical | Lexical control |
|---|---:|---:|
| Synonym / cross-language | 5/5 complete spans | 0/5 |
| Exact key | 5/5 | 5/5 |

This very small corpus demonstrates routing and reachability. Much of it fits
within the evidence budget, so these results do **not** establish dense ranking
precision or performance against many competing entities. The larger conflict
corpus below is a more demanding evidence-selection diagnostic.

[Machine-readable source report](../../eval/reliable-recall/results/followup-semantic.json):
indexing 136.7 ms; embedder load 686.2 ms. `/usr/bin/time -l` recorded 750,092,288
bytes maximum RSS and 402,359,616 bytes peak memory footprint. Model weights and
OS cache were already available. Reported query percentiles are diagnostic
loaded-model observations, not production or process-cold p95 estimates.

### Reader: 160 actual local calls

[Twenty frozen cases](../../eval/reliable-recall/followup-reader.jsonl), four arms
and two budgets produced 160 captured responses. Eighteen cases require source
spans, including competing evidence for clarification; two have no supported
answer.

| Diagnostic | 2,000 tokens | 3,000 tokens |
|---|---:|---:|
| Retrieved complete required spans | 4/18 | 5/18 |
| Lexical-control complete spans | 1/18 | 1/18 |
| Retrieved expected action label | 8/20 | 9/20 |
| Gold expected action label | 14/20 | 14/20 |
| No-evidence abstention | 20/20 | 20/20 |
| Retrieved syntactically malformed/truncated JSON | 0/20 | 2/20 |
| Retrieved citations contained in delivered coordinates | 2/20 | 4/20 |
| Gold citations contained in delivered coordinates | 18/20 | 18/20 |

The retrieved exact-answer-with-provenance diagnostic was 0 at both budgets;
the gold arm was 1 at each. Those low exact-match counts are **not** a semantic
answer-correctness estimate: prose paraphrases fail exact equality. Citation
containment only checks source identity/range; it does not prove entailment.
Some retrieved responses copied the entire evidence header into `citation.source`
instead of the source reference, so the strict source-coordinate check rejected
them. This protocol failure is retained in the captures rather than repaired by
the evaluator.

Manual inspection of the 2,000-token retrieved and gold answers found concrete
failures beyond the automatic metrics:

- `condition-1`, gold: recommends `restore-release-53` while omitting the
  health-check-failure precondition, despite complete evidence.
- `conflict-2`, retrieved: chooses `복구-22` while the conflicting instruction
  was not fully delivered. Gold retains both commands but labels the response an
  answer rather than a clarification.
- `scope-1`, retrieved: outputs an unrelated restore command when environment
  evidence is missing. Gold states the ambiguity and gives environment-specific
  branches; an action-label mismatch alone should not erase that useful behavior.
- `multisource-2`, retrieved: corrupts the snapshot prerequisite in Korean.
  Gold retains both prerequisites accurately.

These examples separate evidence-selection loss, reader behavior and citation
protocol errors. Action labels alone do not prove that a targeted clarification
was actually asked. The live quality gate is **not passed**. Independently
annotated project tasks, an external subset and semantic faithfulness review
remain evaluation work; no learned ranker or automatic reinforcement was added.

[Full reader report and captures](../../eval/reliable-recall/results/followup-reader.json):
reader load 2,640.0 ms, embedder load 651.8 ms, indexing 390.9 ms, wall time
438.0 s. External maximum RSS was 5,835,407,360 bytes and peak memory footprint
3,006,960,656 bytes. Some workspace compilation/testing overlapped this quality
run; its latency observations are not isolated performance results.

## Performance measurements and verification

These release measurements ran directly from built test executables, without
concurrent compilation or model inference. The packer fixture has no shortening
variants; different source headers make its absolute counts different from the
original study. Timings are individual observations, not distribution estimates.

| Candidates | Selected | Context tokens | Tokenizer calls, reference → optimized | Tokenizer bytes, reference → optimized | Pack ms, reference → optimized |
|---:|---:|---:|---:|---:|---:|
| 8 | 8 | 496 | 32 → 8 | 28,624 → 7,156 | 4.81 → 1.05 |
| 32 | 31 | 1,964 | 128 → 32 | 421,782 → 105,546 | 61.26 → 15.21 |
| 64 | 31 | 1,964 | 256 → 64 | 1,229,526 → 310,698 | 181.60 → 45.45 |

All serialized results were identical to the frozen reference. Calls fell 75%;
counted bytes fell approximately 75%. Final result serialization was measured
separately at 0.004–0.015 ms in this fixture (this excludes intermediate
serialization already included in packing time). Multi-layer, non-additive
counting also has an independent correctness regression.
[Full packing observations](../../eval/reliable-recall/results/followup-pack-costs.json).

The source-query cost fixture keeps an older target session plus ten unrelated
sessions. Two model reopenings took 621.4–622.5 ms; loading plus the first query
took 644.6–646.0 ms. Six loaded-model queries, including each first query, took
18.9–23.5 ms and retained the target. This is a cold-model / warm-OS-and-index
measurement in one process, not process-cold latency. Single-item and batched
embedding outputs were identical in a separate three-text sanity check. The
combined tokenizer/model cost process peaked at 735,887,360 bytes RSS and
393,954,600 bytes memory footprint.
[Query costs and embedding probe](../../eval/reliable-recall/results/followup-query-costs.json).

The truth fixture holds the requested group fixed and uses a 1,100-byte
field-size-estimate allowance. Each scale ran five queries:

| Catalog groups | Query median ms | Materialized groups / sources | Guarded bytes | Final selection ms | Final fold ms |
|---:|---:|---:|---:|---:|---:|
| 10 | 2.45 | 2 / 2 | 896 | 1.16 | 0.0051 |
| 100 | 5.71 | 2 / 2 | 896 | 4.36 | 0.0033 |
| 1,000 | 44.60 | 2 / 2 | 896 | 43.62 | 0.0080 |

Every query retained the requested group. The fixture also tests a historical
replacement and rejects an oversized complete group rather than returning its
winner alone. The full scale process, including creation of all three stores,
peaked at 31,440,896 bytes RSS and 23,642,616 bytes memory footprint. This is a
process measurement, not an assertion that the field-size guard bounds all heap
allocations. Linear metadata selection dominates the 1,000-group query; the
change fixes query-independent exclusion without claiming sublinear scaling.
[Scale observations](../../eval/reliable-recall/results/followup-truth-scale.json).

Final local verification uses Rust 1.99.1 on Apple Silicon, based on
`40482ee71fa7d108fc5f48f09929ffe9eb2fbcf6` plus the working-tree changes:

- `cargo test --locked --workspace --all-features`: 1,065 passed, zero failed,
  eight explicit/environment-dependent measurements ignored. Includes PDC
  corpus tests, byte-identical truth reprojection, writing-system parity,
  redaction races, migration chain, CLI/MCP and native adapters.
- After the source-count receipt addition: 22 relevant facade/store integration
  tests passed; both complete-group-selection and paged-dense-top-k property
  tests passed. The three new ignored release measurements were explicitly run
  and passed separately.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo deny check`: advisories, bans, licenses and sources passed.
- `cargo build --locked -p oxibrain --no-default-features --features http-llm`:
  passed. The default facade dependency tree has no `oxios-` or `oxicode-` crates.
- `bash scripts/publish.sh --order-only`: passed, including the shared native
  runtime before both adapters. This check publishes nothing.
- `bash scripts/check-consumer.sh`: passed with a fresh consumer lockfile,
  including both local adapters and the shared runtime.
- `git diff --check`: passed; both frozen corpus hashes verified.

The checkout already contained Rust/CI/rendering/index changes and research
artifacts before this task. They were preserved; verification covers the combined
checkout rather than attributing all of its changes to this follow-up.

## Reproduction

From the repository root, configure the absolute existing tokenizer and local
embedding paths and absolute JSON output destinations, then run:

```sh
cargo test --release --locked -p oxibrain-cli --test followup_costs -- --ignored --test-threads=1
cargo test --release --locked -p oxibrain --test truth_selection measure_truth_selection_scale -- --ignored --nocapture
```

The first command uses `OXIBRAIN_TOKENIZER_JSON`, `OXIBRAIN_EMBED_MODEL`,
`OXIBRAIN_PACK_OUTPUT` and `OXIBRAIN_QUERY_COST_OUTPUT`. The second uses
`OXIBRAIN_SCALE_OUTPUT`. To measure process memory, run the resulting test
executables under `/usr/bin/time -l` after building them; wrapping compilation
would mix compiler and query memory.

For quality replay, set `OXIBRAIN_EVAL_CONFIG` to an explicit JSON configuration:

```json
{
  "corpus": "eval/reliable-recall/followup-reader.jsonl",
  "tokenizer": "/absolute/path/to/tokenizer.json",
  "embed_model": "/absolute/path/to/bge-m3-Q4_K_M.gguf",
  "output": "/absolute/path/to/reader-results.json",
  "live_reader": true,
  "budgets": [2000, 3000],
  "reader_tokens": 384
}
```

With the existing pinned Qwen model configured locally, explicitly select
`OXIBRAIN_LLM_PROVIDER=local` and the appropriate `OXIBRAIN_MODELS_DIR`, then run
`target/release/oxibrain admin eval --suite reader`. Use the semantic-controls
corpus and `--suite recall` for reader-free paired source coverage. Preserve the
frozen manifests and keep subsequent tuning separate from these diagnostics.
No deployment, publication, production-store migration or document conversion
was performed in this implementation task.
