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

## Third candidate and consumer checks

Commit `8549de97b49fd6f10dc670048e3a3e393c716b48` changes normal reads to
`read-policy/4`, using compact `context_render/2` by default. Complete source
coordinates, roles, supports, uncertainty and omission metadata remain available
out of band. Explicit `compact_evidence=false` requests full framing; the old
`pack_evidence` compatibility helper retains full framing for its frozen baseline.
The policy-aware packer has an exact-budget/source-map regression test.

The independently frozen v3 subset excludes all 40 v1/v2 question IDs. Its
20 cases contain 931 sessions, 9,638 turns and 20 annotated answer turns. The
compiled default-feature runner was copied before launch and frozen with the
candidate commit, exact compact policy, dataset/model/tokenizer hashes and both
budgets in `eval/research/longmemeval-s/results/v3-compact-freeze.json`.
Root has not inspected v3 questions, answers or case labels. Gold loading occurs
only after every retrieval request. The run is pending at this checkpoint.

`evidence-reader/4` permits at most two attempts for invalid JSON or a delivery
contract failure. The retry retains the question, evidence, model, output budget,
schema and grammar; it adds typed diagnostics and the prior reply as an untrusted
JSON string. It never receives gold or expected action labels. Provider failures
are not retried. Every request, response, validation error and latency is captured;
the final response passes the same validator or exposes an explicit failure.
Eight reference-reader unit tests, seven skill tests, sixteen packer tests and
five external-profile tests passed (one external execution test remains opt-in).

The release executable generated and installed the actual skill into a temporary
Codex directory. `skill-creator` validation passed. A fresh independent consumer,
given no expected answers, handled two new offline evidence scenarios: it kept
both conjunctive rollout prerequisites, rejected cross-project approvals, and did
not infer entity identity or transfer policy from an incidental nickname. The
generated-file/binary hashes, complete responses and scope of this check are in
`eval/research/skill-forward/v021-final-forward.json`. This checks those consumer
answers, not MCP transport, retrieval quality or universal agent compliance.

All-target/all-feature clippy passed on this candidate, as did the cached CLI
extraction evaluation. A workspace test run reached doctests but failed to load
`oxibrain_connectors` (`E0463`) while another feature build shared its target
directory. A serial full rerun is in progress; the failed invocation is not
reported as a passed suite.

The serial full all-feature workspace rerun passed 1,149 tests, zero failures
and 11 explicit ignored tests across 121 suites, including doctests. The CI run
at `07cb4b7` also passed every job, including Linux MSRV 1.96 and the frontend.

A release reviewer found two additional publication defects: process substitution
hid dependency-order computation failures, and source-file mtimes made archive
bytes change on an otherwise identical draft-upload retry. Publication now checks
and rejects a failed/empty order before any package upload. The two binary
archives use the tagged commit's timestamp, normalized tar ownership/mode and a
zero gzip timestamp. Exact comparisons still reject genuinely different existing
assets. Root independently passed metadata/cycle/empty-order fixtures,
archive reproducibility checks, the existing release verifier tests, actionlint
and shell syntax checks. These changes do not alter the frozen retrieval binary.

## Owner-selected local 4B and evaluation adapter repair

The owner explicitly selected local 4B and rejected further use of Dyno's heavy
35B model. The existing Dyno service subsequently reported `llm=false`; no further
35B generation is requested. Completed comparisons remain historical evidence.
The shipped default extractor is now Qwen3-4B-Instruct-2507 Q4_K_M, pinned to
HF revision `a06e946bb6b655725eafa393f4a9745d460374c9` and BLAKE3
`8712872ae483c8d3d4c51c15e44663bd05cbbc08dfefe56a9c6feec38b9f0c78`.
The artifact was already installed; no weights were downloaded. The local
manifest's existing 4B URL was pinned and an incorrect pre-existing BGE digest
was corrected after backing up the manifest. Both named artifacts pass CLI
verification. Alternative model files and unrelated service configuration stay
available; they are not selected for this work.

`evidence-reader/4` completed 182 actual native-4B calls across the two budgets
(including retries). It validates 16/20 retrieved replies and 16/20 gold replies
per budget, with valid abstention for all 20 no-evidence controls. A fresh blind
AI review judged 10/20 retrieved replies adequate, four inadequate, four failed
delivery and two gray/evasive; gold had twelve adequate, four inadequate and four
delivery failures. The full captures and independently returned judgment summaries
are preserved as `v021-reader4-native*.json`. Valid-but-inadequate replies still
include cross-project joins and missing mandatory guards. Delivery validation
does not close that semantic gap.

The already-completed loopback comparison made 166 actual calls: retrieved
delivery 18/20 and gold 20/20 per budget, all no-evidence controls abstaining.
Its blind review found 18/20 retrieved adequate and two delivery failures; gold
was nineteen adequate with one operationally ambiguous unresolved-command choice.
This service does not expose a weight digest, and the owner subsequently chose
4B. These measurements do not change the selected model or qualify its quality.

The next 4B-only diagnostic uses `evidence-reader/5`, aligning its instructions
with the generated skill's same-entity/project scope, explicit identity bridge
and complete conjunctive prerequisite rules. It keeps the schema, grammar,
evidence, strict validator and two-attempt bound. It runs only the 3,000-token
budget because paired 2k/3k requests in the preceding diagnostic were identical;
this is not a new independent sample. Its binary, source/configuration hashes
and model pin were captured before result inspection. Eight reader unit tests,
seven model-catalog tests and the provider-selection tests passed.

The first v3 external execution reached case 17 and failed before gold loading:
the adapter incorrectly required globally unique upstream session IDs. No
complete report or score was produced. Structural-only inspection found two
different source occurrences with that ID at input indices 1 and 27; their
date/body hashes differ. The failure is preserved in `v3-first-failure.json`.
The adapter now preserves every occurrence, adding a deterministic collision-free
ordinal suffix only for repeated IDs. Unique IDs keep their existing references.
Gold still loads after all retrieval requests. Exact session/turn indices map
annotations; ambiguous duplicate-ID session annotations fail rather than choosing
a favorable copy. Counts expose both upstream IDs and occurrence denominators.
Eight adapter tests passed, including identical-byte duplicate occurrences,
ambiguous mapping rejection and unchanged unique-ID identity. The raw v3 input,
gold, budgets, segmentation and retrieval policy are unchanged for the rerun.

The 4B-only prompt-5 diagnostic made 98 requests at 3,000 tokens: retrieved
delivery was 14/20, gold 16/20 and no-evidence abstention 20/20. Its independent
blind review found eight retrieved adequate, three inadequate, six delivery
failures and three gray replies; gold had fourteen adequate, one inadequate,
four delivery failures and one gray reply. Longer general instructions did not
improve this retrieved-evidence diagnostic, so the implementation restores
`evidence-reader/4` rather than retaining the experimental prompt. Prompt-5
source identity, requests, responses and judgment summaries remain available in
`v021-reader5-native*.json`; its gold-only improvement does not establish a
general prompt ranking. The generated skill keeps its independently exercised
scope and conjunction guidance.

The selected 4B also passed the real grammar-constrained extraction smoke:
three existing English fixtures, all five expected relations, precision/recall
1.0 and fabricated-entity rate zero. This uses the pinned local artifact, not a
canned provider; it is a small extraction check, not a general answer benchmark.
The complete report is `v021-4b-full-smoke.extraction.json`. The companion reader
smoke has one known case and is not added as an independent quality sample.

At `78cba7c`, the all-feature workspace run passed 1,152 tests with zero failures
and eleven explicit ignored tests across 121 suites. After restoring the prior
reader prompt, all 68 CLI unit tests passed. CI at `78cba7c` passed every job,
including MSRV and frontend checks. The retained reference reader still exposes explicit
delivery limitations; the evaluation client is not a new automatic-answer engine.

## Smaller source units and bounded source context

The occurrence-preserving v3 run completed at `78cba7c`. At both 2,000 and
3,000 tokens it fully retrieved 17/20 annotated answer turns (85%), with
6,661/7,500 annotated bytes (88.81%). This fails the frozen 90% whole-turn
target. All answer sessions were retrieved for 11/12 answerable cases; at 3k,
at least one answer session was found in all twelve. Session coverage and byte
coverage do not replace whole-turn coverage. The full report is preserved as
`v3-occurrence-compact.json`.

V3 is now a development diagnostic. A separate agent inspected its three
missing turns: one was at the beginning of an otherwise retrieved session;
two were mixed into large windows with unrelated dialogue. The next generic
candidate uses 1,024-byte paragraph/line windows (128-byte overlap for oversized
lines) and a bounded source prefix after the first ranked later query hit.
No benchmark role/turn markers, languages, answer strings or per-case exceptions
define this behavior. Schema 18 invalidates only rebuildable raw ranking state;
read policy 5 records candidate context bytes independently from final packing.

Before running this candidate, a separate agent froze v4 by strictly excluding
all 60 prior source question IDs. Only six unseen abstentions remained, all in
the multi-session category. The preregistered subset therefore contains eighteen
cases: two answerable cases per each of six categories and those six abstentions.
The manifest records this quota deviation and makes no reader or abstention
success claim. Its 895 full-history sessions, 9,115 turns and 25 annotated answer
turns remain separated from retrieval input. Root has not inspected its questions,
answers or individual labels. The external runner loads gold only after all
retrieval requests complete. Frozen hashes are in `longmemeval-s/v4/manifest.json`.

The targeted candidate run passed 287 tests across six suites (zero failures,
one explicit external-run ignore), including prefix planning properties, every
source scope exclusion, exact-budget facade packing, historical migration and
the occurrence adapter. A separate read-only reviewer found no material defect;
its cursor-version test correction is included. Comparing actual frozen metadata
also found a truncated v1 input hash constant introduced while adding profiles.
The authoritative input and prior complete report agree; the constant is fixed,
and profile validation now checks actual v1/v4 manifests rather than only
self-generated fixtures. No historical report or corpus was changed.

A later environment check found an old `OXIBRAIN_LLM_PROVIDER=loopback`
export in the owner's `.zshenv`. The local model catalog alone could not override
that explicit selection. After private backups, the Oxibrain-specific shell
provider was changed to `local` and its obsolete loopback URL/model exports were
removed. New shells report the local provider. Codex's existing Oxibrain stdio
server also now has an explicit `env.OXIBRAIN_LLM_PROVIDER="local"`, using the
[documented per-server environment setting](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).
The parsed Codex configuration was compared before/after to ensure that only
that server's environment changed. Existing MCP processes are not evidence of
the new configuration until they reconnect; no unrelated model service was
stopped or reconfigured. All subsequent native extraction checks explicitly
select local inference and the verified 4B artifact.

The first v4 run completed all eighteen retrieval requests and attached
evaluator-only gold metrics, then failed while constructing the report: a
remaining hard-coded `source_rows_scanned` access did not use v4's frozen
`source_rows_scanned_per_pass` metadata key. No complete report or score was
produced. The 328.00-second attempt and exact error are preserved in
`v4-prefix-first-failure.json`. Report construction now shares the same
profile-aware source-scan accessor used by preflight validation, which is
exercised against actual frozen v1/v4 manifests. The rerun keeps input, gold,
retrieval implementation, model, source windows, policy and budgets unchanged.
Root has not inspected v4 questions, answers or individual case labels.

The complete local all-feature workspace run at `26c7ace` passed 1,162 tests
with zero failures and eleven explicit ignores across 122 suites. Clippy with
all targets/features and warnings denied passed, as did fmt and cargo-deny.
CI at `26c7ace` and metadata-only `fa445e5` passed every job, including MSRV,
standalone dependency isolation, fresh consumers and the console bundle.
An isolated restore of the verified schema-15 production backup upgraded to
schema 18 and rebuilt raw lexical indexes. All rows and columns of all nine
ledger/truth tables compared identically before/after; SQLite integrity was ok.
This operation loaded no model and did not open or modify the production store
or source documents. The private proof is retained outside the repository.

The report-repaired v4 execution completed with exit zero in 312.48 seconds
(maximum resident set 1,176,895,488 bytes). All twenty-five annotated answer
turns were fully retrieved at the frozen 3,000-token budget, covering all
6,978 annotated bytes and all annotated answer-session occurrences in all twelve
answerable cases. At 2,000 tokens it fully retrieved 23/25 turns (92%),
6,734/6,978 bytes (96.50%), and every answer-session occurrence in eleven of
twelve answerable cases; each of the twelve had at least one answer session.
Both budgets meet the frozen 90% whole-turn target on this subset. Actual token
spend never exceeded the declared budget. The complete capture is
`v4-prefix-repaired.json`; both failed attempts and pre-execution freezes remain.

V4 contains eighteen questions, 895 full-history sessions and 9,115 turns. Its
twenty-five annotated turns are a small English-only sample from a public
dataset with simulated source dialogue. These are retrieval measurements,
not a reader, abstention-success or multilingual answer-quality score. The
same-case failed reporting attempt did not expose scores to root or change the
retrieval implementation. Differences from v1/v2/v3 are not a paired causal
effect estimate. The broad automatic-answer quality gate remains open, and the
retained reference reader continues to reject invalid delivery explicitly.

The final known-source regression also passed: all eighteen answerable cases
retain every required support span at both 2k and 3k, including Hangul and Latin
cases, conditions, conflicts, identities and applicability. Two missing-answer
cases remain controls, not positive retrieval successes. The lexical-only
control reaches 17/18; default local hybrid recall reaches 18/18. Captures and
the pre-execution freeze are `v021-final-source-recall*.json`. These repeated
synthetic diagnostics are not added to the independent external sample.

The final candidate's native 4B integration smoke completed with the verified
`8712872a...` artifact and `evidence-reader/4`. Three existing extraction fixtures
again produced all five expected relations, precision/recall/F1 1.0 and
fabricated-entity rate zero. The full source and extraction reports are
`v021-final-4b-smoke*.json`. Its companion one-case reader exercise is a repeated
integration check, not a new answer-quality sample. No Dyno/HTTP provider was
selected for these final checks.
