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
