# Recall release readiness: research and implementation record

Date: 2026-10-24. Status: in progress; no release or quality acceptance claimed.

The owner authorized further research, implementation, included-skill improvement,
subagent work and deployment. [The readiness specification](../spec/release-readiness-v1.md)
records the scope and gates. The prior
[failed live evaluation](2026-10-24-post-m12-followup.md) remains unchanged evidence.

## Source-grounded findings

The document distance implementation dequantized int8 blobs into fractional
floats and cast those floats back to int8, eliminating most model dimensions.
The quantized query's signed bytes were also compared as unsigned values. A
single pure signed-int8 distance function now replaces those calculations in both
entity and document KNN. Self-distance and realistic small mixed-sign embeddings
are tested; malformed dimensions/NaN inputs produce typed errors.

A separate read-only subagent identified strict whole-question AND in document
FTS and heading expansion requiring all question words in the heading. Those
findings are being addressed through bounded component queries and structural
section selection. Its initial claim that evaluation noise sessions were in the
future was incorrect: 1.85e12 is before request time 1.90e12. That claim is
retracted; it is not evidence for changing temporal semantics.

The optional compact render binds response-scoped IDs to complete source maps.
The reference reader quotes evidence before selecting an action, declares
verbatim conditions and keeps clarification separate from an actionable answer.
Pure validation rejects invented IDs, quotations and inconsistent envelopes. This
checks delivery and structure, not entailment or whether omitted evidence exists.
[ADR-021](../adr/ADR-021-recall-delivery.md) specifies this boundary.

## Research and external evidence

Research used Agent Reach; the Exa route hit its free limit, so the subagent
verified primary papers, official repositories and pinned dataset metadata.

- [LongMemEval v2](https://arxiv.org/html/2410.10813v2) separates retrieval and
  reading, updates, temporal reasoning, multi-session inference and abstention.
  Its histories are generated and screened/edited; this is external curated
  benchmark evidence, not a natural-user deployment sample.
- [Original RRF paper](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)
  motivates rank fusion as a baseline across unlike score scales; its experiments
  do not guarantee the best weights for this corpus.
- [ALCE](https://arxiv.org/html/2305.14627) distinguishes citation correctness
  and coverage. An existing ID or a contained quotation is not proof that the
  answer follows from it.
- [MemConflict](https://arxiv.org/html/2605.20926v1) and
  [TANGLE](https://arxiv.org/html/2608.13921v1) motivate separate inspection of
  updates, conditional applicability, unresolved conflict and clarification.
- [MIRACL](https://github.com/project-miracl/miracl) supplies a useful external
  multilingual retrieval task, but its Wikipedia corpus and varying language
  difficulty cannot establish matched conversational-memory parity by themselves.

A research subagent independently froze 20 cases (12 answerable, 8 abstention)
from the official MIT-licensed cleaned LongMemEval-S dataset at HF revision
`98d7416c24c778c2fee6e6f3006e7a073259d48f`. The selected 943 sessions contain
10,104 turns. Gold annotations are separate and absent from promptable inputs.
Root has not inspected the answer labels or case content for tuning.
A preparation schema bug initially omitted the parallel session IDs/dates; it was
corrected before evaluation without changing selected cases or their order.
The input SHA-256 after that repair is
`3d098e4e4aa0f0c2a9a6f279921b23d25ab7c81d94432e57983975a69a0d06ca`;
gold is `a2e0f50dd2cc530454b24155d8da0701d54d4c15880400d1147f1268c1dfcf49`.
The final manifest and preparation helper retain upstream identity, hashes,
selection logic and the verbatim license. This small English-only subset has
limited statistical power and is not comparable to the official GPT-4o-judged
answer score without that evaluator.

## Release preparation

The existing published version is 0.20.1. The working release is 0.21.0; no old
tag/assets will be replaced. Workflow publication requires a matching checked-out
tag, exact-version consumer smoke, staged draft assets, download/checksum/archive
verification and promotion only after those checks. The established CLI macOS
archive channel currently does not claim Apple Developer ID signing/notarization.

Final quality measurements, regression results, skill forward tests, commit/PR,
registry and asset identities, and post-install checks will be appended after
actual completion.

## First external execution: blocked by native encoder safety

The first input-only memory-plane run used the frozen production default policy
and the debug integration harness (SHA-256
`fdedc124d63f75ff653b375c4fc8c1ff235af137cc0c30b0fb7f485ceef93398`).
It aborted in case 1 before gold annotation loading or any retrieval report.
The pinned llama.cpp encoder asserts `n_ubatch >= n_tokens`: its default physical
micro-batch cannot process a complete source sequence exceeding that limit even
when `n_ctx` permits it. This is a product crash, not a failed benchmark score.
The failure and freeze metadata are retained; no answer labels were inspected to
fix the adapter. The repair must validate length before FFI and size the encoder
batch for complete sequences without silent truncation or causal-style splitting.

The independent generated-skill forward test handled three synthetic situations:
conditional runtime/tenant guidance, identical text with distinct source identity,
and a capped empty result. It preserved separate sources, asked for missing scope,
ignored injected source instructions, and did not infer absence from an empty read.
This verifies those exercised behaviors, not arbitrary consumer compliance.

## First completed external run and further diagnostics

After the native adapter repair, the first default-policy v1 run completed all
20 cases. At 2,000 tokens it selected an answer session in 11/12 answerable cases;
at 3,000 tokens in 12/12. All answer sessions were selected in 10/12 at both
budgets. These are session-level observations, not supporting-turn coverage.
The returned coordinates covered 5,855/11,229 annotated answer-turn bytes
(52.14%) at both budgets, below the predefined 90% target. No reader or
abstention score is inferred from this run. The original report remains in
`eval/research/longmemeval-s/results/default-repaired.json`; v1 is now a
development diagnostic because its aggregate result has been examined.

A reviewer found that the generic packer suppressed every partially overlapping
source window. For example, accepting `0..2048` rejected `1792..3840`, including
a condition at byte 3000. It now suppresses only fully contained ranges with the
same complete source identity. Partial overlaps and supersets remain eligible
and pay their actual rendered token cost. The frozen old packer is retained as
a baseline. Fifteen evidence-pack tests passed after this repair.

The raw-term planner now deduplicates case-insensitively before applying the
query-term cap, preserves first spelling, bounds input examination, and reports
its plan. Channel result limits conservatively mark coverage partial. The new
planner/property tests and three semantic recall integration tests passed.

A separate review demonstrated a family-count bias: every term generated two
lexical lists, while the dense full-question result had one vote. Sixteen broad
lexical lists could overwhelm a narrow dense hit by nearly three to one. The
next candidate balances lexical and dense rankings as retrieval families after
within-lexical fusion. This is a structural correction with a tradeoff: an
incorrect dense ranking also gets more influence. Benchmark improvement must
be measured, not inferred.

The initial `evidence-reader/2` run parsed JSON but validated 0/20 retrieved
replies and 4/20 gold-only replies at each budget; the no-evidence controls
abstained validly in 20/20. Failures included IDs copied together with role
headers, paraphrased conditions, and conditional actions with an empty answer.
This rejects malformed delivery safely but fails the stronger generation goal.
`evidence-reader/3` separates IDs, roles, and source text as JSON fields, states
all action invariants, and returns typed validation failures. It does not repair
invented IDs or relax exact quotation checks. The revised model run is pending.
The newer protocol also uses 768 output tokens versus 384 in the old baseline;
these runs do not isolate a prompt-only effect.

An independent subagent froze a disjoint 20-case v2 subset before root inspected
any questions, answers, or case labels. Selection uses the fixed seed
`oxibrain-release-0.21.0-unseen-v2`, excludes every v1 question ID, and preserves
category quotas. It contains 980 sessions and 10,126 turns, with 12 answerable
and 8 abstention annotations. The pinned helper, metadata and MIT license are
in `eval/research/longmemeval-s/v2/`; raw input and evaluator-only gold remain
outside the repository. The next final candidate will be frozen before running
that subset.

The next candidate also versions generic raw-source granularity as
`source-paragraph-window/2`: consecutive complete paragraphs are grouped up to
2,048 bytes; oversized paragraphs prefer complete lines; oversized lines use
UTF-8-aligned windows with overlap. Exact source bytes and offsets remain
unchanged. Schema 17 clears only derived raw chunks, FTS/vector rows and index
cursors so explicit indexing rebuilds them from the ledger. The migration has
a v16 up-test for truth preservation and cache invalidation. This does not
recognize benchmark turn markers or add language-specific parsing.

## Validation after the second candidate

The final all-feature workspace run passed 1,141 tests with 11 explicit ignored
model/scale/external tests. Targeted native testing separately loaded the pinned
Qwen GGUF and confirmed an over-context prompt returns a typed error before
context creation/decode. Six pure generation-capacity boundary tests passed.
The new paragraph-window property test found a tiny UTF-8 boundary regression
(`A0aਵ`, byte limit 2, overlap 1); the generic window helper now skips ranges
that add no coverage, and the deterministic regression/property tests pass.

Rust 1.96 all-target/all-feature checking passed on Apple ARM64 after selecting
`/bin/bash` for native CMake helper commands; the earlier Homebrew Bash helper
processes had stalled and were stopped. The independent Linux MSRV job and all
CI jobs passed at `631e931`. The standalone HTTP-only build and a genuinely
fresh consumer dependency lockfile also passed.

A verified schema-15 backup was restored into an isolated temporary store,
migrated to schema 17, and explicitly reindexed. All nine ledger/truth tables
were compared as complete sorted row values and remained identical; SQLite
integrity passed. Private store contents, row counts and digests are not part of
this public report. The production store has not yet been opened by the new
binary at this checkpoint.

The revised `evidence-reader/3` with the pinned 4B GGUF validates 14/20 retrieved
replies and 15/20 gold replies; all 20 no-evidence controls abstain validly.
There were 160 actual calls, but all 80 paired 2k/3k requests were byte-identical;
the budgets are not independent evidence or extra distinct cases. An independent
AI reviewer, without expected answer/action labels, judged 10/20 retrieved and
11/20 gold replies adequate and valid. Six/five replies failed delivery, and
four valid replies per arm were inadequate. Material errors include unsupported
entity joins, cross-project claims and omission of a mandatory prerequisite or
health-check condition. This does not qualify the 4B reader as a reliable
automatic-answer tier. The engine still reports answerability as not evaluated;
the reference reader is an evaluation client, not a truth-producing engine path.

An additional exploratory run uses the already-running local Dyno loopback
service reporting `mlx-community/Qwen3.6-35B-A3B-4bit`. The API does not expose
its serving-weight digest, so this run cannot claim a cryptographically pinned
reader artifact or transfer quality to the default GGUF. No new weights or paid
provider were requested. The external v2 retrieval run retains its frozen
candidate commit/binary and evaluator-only gold; both experiments are pending.

## External v2 result

The frozen second candidate completed the disjoint v2 input-only retrieval run.
At 3,000 exact tokens it retrieved all answer sessions in 12/12 answerable cases
and covered 9,224/9,932 annotated answer-turn bytes (92.87%). Complete answer
turns were 21/24 (87.5%), so the 90% whole-turn target was not met. The initial
byte-only acceptance claim was corrected before release. At 2,000 tokens, 11/12 answerable
cases had all answer sessions and coverage was 8,043/9,932 bytes (80.98%). Complete turns were 17/24 (70.83%) at 2,000 tokens; the smaller budget
also does not meet the target. Neither number is answer correctness
or abstention behavior; no reader was used in this external retrieval run.
The different v1/v2 cases prohibit a causal 52.14% → 92.87% improvement claim.

The source candidate is commit `631e931a83bd24051b892fdaa8c2a40a0dfe78da`;
the frozen integration executable SHA-256 is
`3b9d757bd8f4bdd255c68088adc9446d09ad7ae560a8e4561191fa33bd3fe76c`.
Its binary, model, tokenizer, policy and dataset pins are in
`eval/research/longmemeval-s/results/v2-candidate2-freeze.json`; complete
coordinate/receipt measurements are in `v2-candidate2-default.json`. The root
read only aggregate scores after all requests and evaluator gold loading
completed. The subsequent candidate changes the compact-render default; it is evaluated
on a newly frozen, disjoint v3 subset rather than relabelling this v2 result.

Preparation deviations remain explicit: an unavailable system-Python helper
prevented initial freeze-file writing, then the first runner launch failed on a
missing external manifest before input parsing/model load. Already pinned
metadata was copied beside the unchanged input/gold, and the freeze file was
written before the actual retrieval run. Neither failure exposed scores or
changed candidate policy. Other CPU checks and a separate local-reader run
overlapped, so elapsed times are descriptive rather than isolated benchmarks.
