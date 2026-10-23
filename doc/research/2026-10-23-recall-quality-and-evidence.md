# Making recall dependable under a bounded context

Date: 2026-10-23 (Asia/Seoul). Status: research and proposed work; no roadmap or architecture decision is adopted by this report.

The next milestone should make the evidence already stored by oxibrain reliably reach its consumers. The highest-value work is public-interface correctness, query-relevant session retrieval, source-sized excerpts, and an evaluation that measures complete evidence under a token budget. These have direct implementation and runtime evidence. More elaborate memory learning remains an experiment after this foundation is measurable.

## Evidence boundary

- Inspected oxibrain v0.19.0 and architecture v2.23. The starting checkout was `0160be9` with existing document-cache changes. During the study those changes became commit `1a14f2d`; their two file hashes stayed unchanged. This investigation made no production-code changes.
- Read the current architecture, roadmap, experience-memory spec, PDC adoption spec, and the installed Portable Document Contract 2 and adoption guidance. Existing public-benchmark deferrals remain in force.
- Built the current CLI, ran its deterministic evaluations, ran 12 targeted integration tests, and exercised five synthetic scenarios through the actual CLI/MCP dispatcher. All experimental stores and source documents were isolated under `/tmp/oxibrain-research-01a16856/`.
- External findings below are claims from primary papers, including preprints, accessed on 2026-10-23. Their systems and numerical results were not independently reproduced here. No live extraction-quality run, actual model-token budget measurement, large-corpus load test, or downstream agent-task experiment was performed.

## What the current checks establish

| Check run in this study | Observed result | Interpretation |
|---|---|---|
| `cargo run --locked -p oxibrain-cli -- admin eval --suite fast` | Fabrication 0.000; statement precision/recall 1.000/1.000 | Three hard-coded, canned-response fixtures containing five expected triples. A useful validator/projection regression check. |
| `target/debug/oxibrain admin eval --suite gate` | Hybrid search c: 42/42; lexical b: 36/42; assembled recall r: 36/42 | Answer-token reachability in this small corpus, with gold declarations supplied directly. These are not generated-answer accuracy scores. |
| Same gate, graph category | b 3/9; c 9/9; r 7/9 | Graph expansion helps this query structure, but two reachable answers disappear on the recall path. |
| Same gate, update / temporal categories | c 20/20 and 13/13; r 18/20 and 11/13 | Recall loses useful evidence or query scope. The recall arm does not receive the question's `as_of`; these six overall misses cannot all be attributed to packing. |
| `admin eval --suite experience` | Recall r 14/14; lexical b 8/14; hybrid c 10/14 | Trajectory prose helps the 14 authored questions. Reported r cost was 1,139–1,349 fallback token units. |
| `admin eval --suite experiential` | Summary 2/4; sources 4/4; hybrid 4/4 | Four questions with canned extraction and canned runbook generation. Hybrid ties sources on the measured questions. |
| Five targeted Rust test binaries | 12 passed; 0 failed; 0 ignored | Dense-mode behavior, declaration-based multilingual resolution, reprojection determinism, runbook terminality, and session identity/trust have working regression coverage. |
| `admin eval --suite full` | Exit 1: requires a live provider | The implementation unconditionally returns this error; this command does not currently perform a live run. |
| `admin eval --suite parity` | Exit 1: unknown suite | CLI/documentation mismatch. The separate `parity_corpus` integration tests exist and passed. |

The targeted test command was:

```sh
cargo test --locked -p oxibrain \
  --test parity_corpus --test m7_dense --test session_memory \
  --test runbook_memory --test reproject_determinism
```

The gate's matcher requires each whitespace-separated answer word to occur somewhere in the returned text. It does not require the words to describe one correct relation or to cite the annotated supporting episodes. `supporting_episodes` is deserialized but unused. Its b arm is lexical-only, whereas architecture §17.2 describes a lexical+dense chunk control. Both b and c read the same temporal fold, so their comparison measures retrieval structure rather than the entire value of structured truth. The reported full-context a arm is another text matcher, not a reader-model ceiling; it even scores 12/13 temporal while b and c score 13/13.

Source: [evaluation fixtures and dispatch](../../crates/oxibrain-cli/src/cmd/eval.rs), [gate runner](../../crates/oxibrain-cli/src/cmd/gate.rs), [architecture §17](../ARCHITECTURE.md), [parity test](../../crates/oxibrain/tests/parity_corpus.rs).

## Five failures reproduced through the public operation path

These experiments use `target/debug/oxibrain --dir <isolated-store> <op>` with JSON on stdin. CLI operations dispatch through `BrainServer`, the same handlers used by MCP. Extraction was never requested. The examples test source retrieval, so success cannot depend on a model first converting every source into relations.

| Scenario and setup | Actual observation | Why it matters |
|---|---|---|
| **Indexed document:** register one canonical PDC Markdown file containing `AmberNebula rollback uses restore-release-53`; index it; search and recall `AmberNebula`. | Document search: one hit with the answer and revision URI. Recall: zero layers. Source SHA-256 stayed identical. | An agent's per-turn memory call omits a document that the same public interface can find. |
| **Older relevant session:** record eight sessions dated October 1–8. Only session 1 contains `Umbra rollback uses restore-release-17`. Recall `Umbra rollback` with budget 2,000. | `session_list(limit=20)` contains all eight and the answer. Recall includes sessions 8–4, excludes the answer, and reports `truncated=false`. | Durable retention and recall availability diverge after five newer sessions. The data has not been deleted. |
| **Evidence late in a session:** record a 593-character summary with the answer beginning at character offset 540; query that decision. | Recall emits the first 200 summary characters, omits the answer, and reports `truncated=false`. | Important work or pending items after the introduction can become invisible to recall. |
| **Long execution record:** ingest one 48,735-character untrusted agent trace containing `Umbra` and a rollback instruction after diagnostic text. Recall `Umbra` with budget 2,000. | Zero layers; zero spent fallback token units; `truncated=true`. | The entire episode is rejected instead of yielding the relevant source slice. |
| **Transaction-time filter:** declare a new `ChronoPerson employed_by ChronoCorp` assertion during the experiment; search `ChronoPerson` with `known_at=0`. | Results still contain the new `employed_by ChronoCorp` snippet. | An advertised historical filter is accepted without controlling the public result. |

Raw synthetic probe results are in `/tmp/oxibrain-research-01a16856/probes/`. These temporary artifacts supplement the reproducible setups above; they are not a durable test suite or production-store audit.

### Mechanisms confirmed in source

1. **The public recall handler bypasses document assembly.** `tool_recall` calls `assemble_context_with_hints` at [server.rs](../../crates/oxibrain-mcp/src/server.rs), line 676. The separate [document-plane `Brain::recall`](../../crates/oxibrain/src/document_plane.rs), line 319, attaches document excerpts, but the public handler does not invoke it. Wiring that method alone still leaves its document query lexical-only and its reconciliation freshness result discarded.
2. **`known_at` is advertised but not threaded through search.** [The op schema](../../crates/oxibrain-ops/src/lib.rs), line 49, declares it. `tool_search`, line 634 of [server.rs](../../crates/oxibrain-mcp/src/server.rs), builds a `Query` without it. Core already has a `Filters::known_at` decision and tests in [rank.rs](../../crates/oxibrain-core/src/rank.rs). This is an executor/parity defect under P9, rather than a new temporal feature proposal. Entity snippets also need the same temporal visibility policy as their ranked evidence.
3. **Recall skips configured dense memory retrieval.** [Context gathering](../../crates/oxibrain-store/src/context.rs), line 87, calls `hybrid_query(..., None)`. [The query executor](../../crates/oxibrain-store/src/query.rs), line 773, enables the hybrid dense channel only when an embedder is supplied. A hybrid label therefore does not prove semantic retrieval on this path.
4. **Session candidates are recency-only and prefix-only.** [Context gathering](../../crates/oxibrain-store/src/context.rs), line 242, loads exactly five sessions. Sessions are excluded from the generic episode pool. [Session rendering](../../crates/oxibrain-core/src/pack.rs), line 550, takes only the first 200 characters.
5. **Episode packing is whole-source-first.** [Episode rendering](../../crates/oxibrain-core/src/pack.rs), lines 494 onward, expands top episodes verbatim and breaks at the first episode that exceeds the remaining budget. Episode candidates lose their individual retrieval scores, receive fixed salience, and are later sorted by a salience/recency proxy. The proxy also uses a fixed epoch rather than an explicit query time. These mechanisms deserve separate measurements before assigning each observed miss a cause.
6. **The model reranker has no production call site.** `RerankPort` and [CrossEncoderReranker](../../crates/oxibrain-llm-local/src/lib.rs), line 193, exist, but the inspected facade/transport retrieval path does not call them. The adapter performs one sequential generative LLM request per item. Pure MMR/corroboration rerankers already exist; this finding concerns the model-based adapter.
7. **Loss metadata understates several omissions.** Session-only recall returned one actual layer with `meta.layers_kept=0`; the handler counts only neighborhood and belief layers. Recency exclusions and session-prefix clipping do not set the truncation flag. Rank's conservation property does not automatically account for candidates discarded before ranking or during packing.

## What recent research contributes

The useful comparison is at the mechanism and evaluation level. None of these results establishes that replacing oxibrain's ledger, fold, or local-first boundaries would improve this product.

| Primary source, version checked | Useful finding | Application and limit |
|---|---|---|
| [EPGM: Execution Provenance for Budgeted Agent Memory Retrieval](https://arxiv.org/html/2609.25913v2), 2026-10-06 | Measures exact supporting-span coverage under a token budget. The authors report +19.07 points from source-aligned candidate units versus flat 512-token windows under matched dense scoring; fixing candidates and seed scores, the graph residual adds +4.55 points. | First test source-sized candidates and complete-support metrics. The corpus is synthetic execution data, candidate eligibility also changes, and the downstream strict-answer interval touches zero. This supports an experiment, not a promised oxibrain gain or a mandate to add an R-GCN. |
| [What Should an Agent Remember?](https://arxiv.org/abs/2610.00366v1), submitted 2026-09-30 | Separates retention access from query-time selection; changing both can greatly inflate an apparent retrieval gain. | Measure where evidence disappeared: retained, indexed, considered, retrieved, filtered, packed. Oxibrain's older-session probe is an access/candidate problem even though the ledger retained the bytes. The paper's seeded streaming setting does not establish this product's gain. |
| [MemDelta](https://arxiv.org/abs/2606.29914v1), 2026-06-29 | Model, embedder, and baseline changes can reverse conclusions about memory architecture; recommends matched comparisons and write-cost reporting. | Make b match the documented lexical+dense control, preserve identical reader/prompt/budget, and separate extraction from retrieval. The existing lexical graph ablation remains useful under its narrower label. |
| [LongMemEval-V2](https://arxiv.org/abs/2605.12493v1), 2026-05-12 | Separates static state, dynamic state, workflow knowledge, environment gotchas, and premise awareness through compact context gathering over trajectories. | Expand the existing in-house experience corpus with environment/version shifts and later corrective evidence. Its large public harness remains deferred by the current plan; these categories can inform local cases without adopting that benchmark. |
| [SimpleMem](https://arxiv.org/abs/2601.02553v3), 2026-01-29 | Combines structured compression, synthesis, and retrieval scope chosen from query intent. | Experiment with bounded retrieval plans and multi-view source indexes after public-path correctness. Its compression claims do not establish losslessness of uncertainty or applicability; source bytes and P10 remain authoritative here. |
| [Hindsight](https://arxiv.org/abs/2512.12818v1), 2025-12-14 | Separates world evidence, experience, observations, and opinions, with temporal/entity-aware retrieval and reflection. | Supports explicit evidence roles and source traceability. Oxibrain already has analogous foundational distinctions; additional stores or an opinion reinforcement loop need independent justification. |
| [Hindsight Memory-PRM](https://arxiv.org/abs/2608.29605v1), 2026-08-30 | Uses retrieval/citation audit trails and controlled evidence interventions to estimate memory utility. | Start with offline evidence masking in isolated replay and observed outcomes. Its trained policy results do not show that read counts or model self-reports can safely update oxibrain's truth. |
| [Memory Injection Attacks on LLM Agents via Query-Only Interaction](https://arxiv.org/abs/2503.03704v5), 2026-02-12 | Demonstrates that ordinary interactions can induce malicious persistent memories without direct database access. | Exercise repeated false session/trace claims, summaries, and source-policy changes across replay. Existing Untrusted session provenance is a meaningful defense boundary; source authenticity alone does not prove a claim or outcome. No such attack was executed against a live user store here. |

## Proposed work, in order

### 1. Restore public-interface correctness

**User result:** a per-turn recall can use a note that search finds, and historical queries respect the time requested.

- Turn the document and `known_at` probes into integration tests through CLI/MCP dispatch. Test returned snippets and source visibility as well as candidate IDs.
- Route public recall through a facade operation that gathers both planes and returns source/freshness metadata. Keep document and episode provenance distinct.
- Carry `known_at` through public search, retrieval, target facts, and snippets. Add behavior tests for every advertised filter; schema acceptance alone cannot establish execution.
- Fix layer counts and report concrete candidate/packing loss reasons. Preserve the existing 16-tool surface.

**Acceptance:** the indexed PDC fixture appears in public recall with its revision-bound source; its bytes remain unchanged. The post-epoch assertion is absent from `known_at=0` evidence/snippets and present at an appropriate later time. The same tests pass in CLI and MCP.

This is the highest-priority PDC connector integration work found here. It extends verification of the implemented reader contract; it does not introduce another format-adoption gate.

### 2. Retrieve the relevant source slices

**User result:** an old decision and the useful portion of a long trace remain available within a small context.

- Search session content by query relevance, with a small recency reserve and current-session exclusion. Build any session index from ledger episodes so reprojection can reconstruct it.
- Return query-bearing excerpts with source offsets and enough adjacent context to preserve meaning. A 200-character prefix can remain a display preview, while retrieval uses the underlying content.
- Pack source-sized units rather than entire large episodes. Preserve step/tool-output boundaries and exact provenance. Evaluate an oversized-candidate fallback that can still consider later candidates.
- Retain retrieval evidence through candidate collection and packing. Add an explicit `Timestamp` input for recency decisions through `ClockPort`.
- Use internal source ranges and revisions; document UUIDs remain canonical. Do not assign block IDs, rewrite notes, or convert PDC 1 documents to support retrieval.

**Acceptance:** all three session/trace probes return the required source span within a budget measured by the configured model tokenizer. An oversized first candidate does not starve a later useful candidate. Every considered unit is selected or has an explicit exclusion reason. Current-session exclusion and untrusted provenance continue to hold.

### 3. Measure actual memory utility

**User result:** an improvement means more correct, supported answers or completed tasks at a stated cost.

- Preserve the deterministic suite and its narrow interpretation. Implement an explicit live runner instead of the current `full` stub, with pinned model/tokenizer/extractor identities and captured responses for later replay.
- Add a held-out in-house set with source spans, valid time, transaction time, and required evidence sets. Split by project, time, and environment; prevent the same cases from supplying extraction examples and final measurements.
- Hold reader model, prompt, embedder, source access, and output budget fixed across controls. Compare lexical+dense chunks, source-sized candidates, graph expansion, and optional reranking one change at a time. Include gold-evidence and no-evidence reader controls.
- Report complete required support within budget, evidence coverage, answer correctness with correct provenance, unsupported-claim rate, appropriate abstention, local-model extraction recall/precision, p50/p95 latency, and write/read cost. Report actual tokenizer identity; `char-fallback` results remain explicitly provisional.
- Expand multilingual retrieval/answer evidence beyond the current 14 parity fixtures, while retaining the existing ≤10pp writing-system variance rule. The passing declaration-based resolution test remains valuable within its scope.

For evidence sets, **Full Support@B** means that all annotated necessary source spans reach the consumer within token budget B. A hit containing an answer name is insufficient if its condition, time, negation, or supporting episode is absent. Exact provenance scoring and a reader evaluation answer different questions and should both be retained.

**Acceptance:** publish paired results and uncertainty intervals for the held-out set, including failures. Separate candidate-access loss from retrieval and packing loss. The existing 42-question set can be a regression suite; tuning it to 42/42 recall alone would not establish a general gain.

### 4. Connect and price the retrieval adapters

**User result:** semantically related wording can find the right evidence without unpredictable local latency.

- Gather dense candidates in the facade outside database transaction scope; pass materialized candidates into pure `rank` and `pack` decisions. Avoid moving inference into store decision code.
- Exercise semantic-only paraphrases and multilingual queries through the public recall route. A spy/fake port can prove the channel executes; a real multilingual model measures its usefulness.
- Keep the current lexical/graph fallback explicit. The MLX build's documented llama.cpp embedding limitation needs capability reporting rather than a silent semantic label.
- Compare the current generative reranker against a bounded batch scoring adapter. Cap candidates and inference budget, identify fallback behavior, and keep results in the rebuildable ranking half.

**Acceptance:** a measured paired gain in Full Support@B or supported answer quality at an explicitly recorded latency/memory cost. Retain an adapter configuration only where that tradeoff is useful. No ranking output may influence the deterministic truth fold.

### 5. Make experience conditional and outcomes auditable

**User result:** a runbook helps the current environment and stops recommending a procedure invalidated by later evidence.

The current experience spec deliberately uses environment-qualified task surfaces, such as `deploy (oxibrain-prod)`. Qualification is a caller convention. The shipped evaluation uses distinct qualified surfaces, so it does not establish behavior when callers omit the qualification or when versions change within one environment.

- Add cases with the same task name across environments, missing qualification, changed dependency versions, later successful fixes, and conflicting observations.
- Present prerequisites, applicable environment/version, last observed outcome, failure conditions, and source references as derived retrieval information. Keep ordered procedures in prose under ADR-018.
- Start outcome measurement with consumer-supplied objective evidence, such as a referenced test result. Distinguish this from an agent's unverified statement that its work succeeded. Source policy continues to determine effective trust.
- Evaluate memory utility by masking candidate evidence during isolated replay. Any durable outcome records require a versioned ledger path and reproducible projection; read counts and ranking scores never become truth evidence.
- Preserve uncertainty dimensions and source availability. The current weighted uncertainty score is a heuristic, not a calibrated probability of answer correctness. Return a reasoned evidence state when support is missing, stale, conflicting, or filtered.

**Acceptance:** the held-out consumer task improves with applicable experience, with no increase in wrong-environment procedure use or unsupported claims. Counterfactual evaluation never requires deleting user data. A schema or architecture change, if needed, gets its own reviewed specification before implementation.

## A concrete first implementation batch

1. Add failing public-operation regression cases for the five reproduced scenarios, plus a same-surface environment case. Keep fixture construction separate from the assertions under test.
2. Complete public document recall, `known_at` propagation, and accurate result metadata.
3. Add relevant-session retrieval and bounded source excerpts; preserve identity, trust, source offsets, and deterministic tie-breaking.
4. Re-run the current suites and evaluate held-out evidence completeness with a real tokenizer. Publish misses before adding another retrieval mechanism.
5. Use that measured baseline to decide whether dense recall and reranking earn their added latency, then evaluate environment-aware experience and outcome feedback.

The architectural strengths to preserve are the immutable episode ledger, deterministic truth rebuild, source occurrence identity, explicit trust policy, terminal derived episodes, pure decisions, and PDC's read-only document boundary. The proposed work builds on these strengths and makes their evidence available at the interfaces consumers actually use.
