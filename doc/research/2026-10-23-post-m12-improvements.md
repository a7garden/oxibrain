# Improvements after reliable recall

Date: 2026-10-23 (Asia/Seoul). Evaluated revision: `40482ee71fa7d108fc5f48f09929ffe9eb2fbcf6`, oxibrain 0.20.1, Rust 1.96.1. Status: research and proposed work; no product implementation or architectural decision is adopted here.

The next work should improve which evidence reaches the reader: expand a matching document heading into its relevant section, apply space scope before dense candidate limits, and connect optional semantic retrieval to the actual recall paths. The previous five M12 defects have already been addressed. Repeating that earlier backlog would misrepresent the current repository.

The research also confirms a query-independent truth-input cutoff and substantial repeated tokenization. These should be measured at realistic scale. The live answer-quality acceptance remains open; source delivery alone does not establish accurate answers, appropriate uncertainty, or useful actions.

## Evidence and limits

- Read the current architecture, M12 specification, implementation report, prior systems research, retrieval orchestration, document search, truth-input loading, packer, and evaluation runner.
- Re-ran 18 existing integration tests at the evaluated revision: reliable recall 12/12, writing-system parity corpus 5/5, and byte-identical truth reprojection 1/1. This is a targeted regression check, not a new full-workspace validation.
- Ran five new diagnostic probes against an isolated detached checkout. They exercise the public Rust facade and pure packer; this study does not newly establish CLI/MCP transport parity. The probes pass their control assertions while recording the observed limitations. They are not acceptance tests asserting that the limitations are desirable.
- Used the existing Qwen3 tokenizer artifact pinned in the [manifest](../../eval/reliable-recall/manifest.json), independently matching SHA-256 `aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4`. No model weights, live inference, paid provider, or external-memory service was used. Deterministic 1,024-dimensional vectors test routing, not embedding quality.
- All probe writes were synthetic and isolated. The heading probe verifies that the PDC source bytes remain unchanged. No production store migration, re-extraction, user-document conversion, or deployment was performed.
- The main checkout acquired unrelated Rust/CI/packer edits during this task. Final probes and regression checks therefore ran on a frozen `40482ee` checkout. Those concurrent changes are not credited to this research.
- External evidence comes from selected method/evaluation sections of three primary papers, not an exhaustive literature review or a reproduced leaderboard. Agent Reach/Exa found sources. Jina returned figure descriptions rather than article bodies, so the primary arXiv pages were read directly.

The [diagnostic harness](../../eval/reliable-recall/followup_probe.rs) and [machine-readable observations](../../eval/reliable-recall/results/followup-diagnostics.json) accompany this report.

## 1. A matching heading needs a route to its supporting section

**Observed.** A canonical PDC Markdown note contains:

```markdown
# OrionRecovery

Approved rollback command: restore-release-53.

Run only after the health check has failed.
```

Searching and recalling `OrionRecovery` yields only the heading. Recall has a 2,000-token budget but emits 149 tokens, with neither the command nor its condition. Searching the command itself retrieves the answer, establishing that the body is indexed and readable. The heading's standalone payload is five tokenizer tokens; the rendered evidence is 149 tokens. These are separate whole-string counts, not an additive token attribution.

This is a structural evidence-selection gap, not a token-budget shortage. The heading result is a complete retrieved chunk, so its citation correctly reports no shortening even though the answer elsewhere in the same section is absent.

**Mechanism.** [`recall.rs`](../../crates/oxibrain/src/recall.rs) turns each document search hit directly into an evidence unit. The document search/materialization path returns the matching chunk; it does not expand a heading match to the section's following blocks. [`quotation_variants`](../../crates/oxibrain-core/src/evidence.rs) can shrink the selected chunk but cannot fetch missing neighbors.

**Proposal.** Preserve small search units, then build bounded section-aware recall candidates from the same revision. A heading-only hit should offer a heading-plus-following-block variant; a short document can offer its complete body. Larger sections should retain prerequisites, negation, and applicability alongside the answer. This is a derived retrieval representation, not a document rewrite or a new canonical format.

**Acceptance.** Title/heading-only queries retrieve both the answer and its condition across Markdown, authored HTML, and readable legacy inputs. Test competing headings, long sections, a negation in the following block, strict token budgets, document changes during expansion, UUID preservation, and unchanged source bytes. Candidate expansion must stay inside the same verified decoded view and revision.

## 2. Space filtering must happen before dense top-k

**Observed.** With one document in the requested space and one in another space, deterministic vectors place the other-space document first globally. A documents-only dense search with `limit=1` returns zero hits. The identical query with `limit=2` returns the correct in-space document. Neither response leaks the other space's content.

**Mechanism.** [`DocumentCache::knn`](../../crates/oxibrain-store/src/documents.rs) scores vectors globally and truncates them. [`search_documents`](../../crates/oxibrain/src/document_plane.rs) fuses and caps candidate IDs before dropping chunks from other spaces during materialization. Thus an unrelated corpus can consume the requested space's candidate allowance. The empty response reports `partial=false`; `answerability` remains explicitly `not_evaluated`.

**Proposal.** Make space eligibility part of the vector candidate fetch, before distance sorting and limiting. A bounded over-fetch loop is a fallback only if its exhaustion is observable. Increasing the global limit alone postpones the problem. Keep eligibility separate from relevance policy and preserve the core/store boundary.

**Acceptance.** Adding arbitrary other-space documents must not displace an otherwise eligible top-k result. Cover K=1, ties, deleted documents, missing vectors, and both dense and hybrid modes. Record a cap-hit/degradation signal where scope cannot be applied before limiting; do not invent an exact unseen-candidate count.

## 3. Configured embeddings do not imply semantic recall coverage

**Observed.** A document about a car's service interval is embedded with a deterministic test adapter. Hybrid document `search("automobile")` returns it; documents-only `recall("automobile")` does not. An old session with the same content, followed by ten unrelated sessions, is also absent from paraphrase recall. That raw-source query invokes the configured embedder zero times. A literal query for its unique service interval retrieves the old session.

**Mechanism.** Document recall explicitly requests `QueryMode::Lexical`. Independent primary/session source channels are word and n-gram FTS. Optional dense claim retrieval operates on extracted structured claims, not on those raw source windows. It also embeds at most the first 256 visible claims in statement-ID order on every call. The receipt says `dense_configured=true` and correctly lists lexical source channels, but that configuration flag does not mean all planes received a dense candidate path.

**Proposal.** First expose per-channel eligibility, execution, coverage, and fallback in the read receipt. Then evaluate independent lexical and optional dense source candidates, including document and session windows. Persist only rebuildable vectors, bound to source revision, decoder/window policy, model identity, and scope. Avoid requiring extraction before a session can be semantically retrieved. Keep lexical operation complete as a supported mode and never download a model during an ordinary read.

**Acceptance.** Use a real multilingual embedder and paired controls for paraphrases, cross-language queries, exact identifiers, competing similar entities, and absent answers. Separate retrieval coverage from final answer quality. Report index size, indexing time, warm/cold query latency, and degradation when the adapter fails. The deterministic probe demonstrates missing routing only; it does not show that a specific model would retrieve the example correctly.

## 4. Choose relevant complete truth groups before exhausting the input budget

**Observed.** A fixture has 24 separate declared facts. Default-limit lexical search can retrieve all 24. Reducing `truth_bytes` to 512 admits one group and reports 23 omitted groups. An explicit query for an omitted person's name still cannot retrieve that person. This is a reduced-cap mechanism test; it does not demonstrate an outage at the default 8-MiB limit.

**Mechanism.** [`truth_input`](../../crates/oxibrain-store/src/read_evidence.rs) walks subject/predicate groups in ID order and applies a space-wide aggregate-size limit before query relevance is considered. [`visible_memory`](../../crates/oxibrain/src/recall.rs) fetches and folds these groups before ranking. The guard correctly excludes incomplete truth groups, but unrelated groups determine whether the desired group is ever considered. Live source IDs and mention surfaces also have space-wide fetches outside that particular group-size bound.

**Proposal.** Build a bounded query-relevant seed set, select group identities in core, and fetch complete competing assertion groups at the requested time scope. Include denials, replacements, retractions, and historical identity inputs. Never optimize by taking only the matching assertion or the current winner. Cached work must be invalidated by the appropriate ledger/exposure/registry revisions; ranking state must never become input truth.

**Acceptance.** Scale from small to large spaces, vary unrelated data while holding the requested group fixed, and test a single oversized group separately. Record groups examined/materialized, bytes, fold time, and source counts. Historical replacement and redaction-race tests must continue to pass. Measure memory use and query latency before choosing any cache or index design.

## 5. Reduce rendering overhead while retaining exact evidence accounting

The pure packer performs repeated full rendering and tokenization. In one debug-build run using the real tokenizer:

| Candidate units | Selected | Context tokens | Standalone payload tokens | Tokenizer calls | Bytes passed to tokenizer |
|---:|---:|---:|---:|---:|---:|
| 8 | 8 | 424 | 80 | 32 | 24,160 |
| 32 | 32 | 1,740 | 342 | 128 | 356,712 |
| 64 | 36 | 1,960 | 386 | 256 | 1,142,824 |

The final 64-candidate context is 6,099 bytes; repeated token counting processes about 187 times that many bytes. This fixture has no quotation variants. The debug timing was approximately 42/671/1,906 ms, but it is not an isolated release-build latency benchmark and should not become a production p95 claim.

**Proposal.** Reuse the unchanged selected-context render/count between attempts, profile serialization separately, and consider a versioned compact citation representation with an unambiguous source map. Preserve evidence roles, revisions, coordinates, and the uncertainty/support bundle. Keep source identity recoverable by the consumer. Final canonical serialization must still be counted with the actual tokenizer; summing per-unit token counts is unsafe because tokenization is not additive across boundaries.

The document reader also reconciles configured roots before a scoped search; root-local reconciliation and shared materialization are additional profiling candidates. Their production benefit was not measured here. The separate in-flight change replacing temporary formatting strings does not remove the repeated-tokenization mechanism measured at `40482ee`.

## 6. Close the live reader gate with conflict and abstention cases

The existing synthetic verification report records 160/160 positive full-support cases at two budgets. That is prior artifact evidence, not a newly repeated measurement in this study. The frozen cases share generated templates, and the 40 no-answer cases do not measure abstention without a reader. The existing full runner compares retrieved, legacy, gold, and no-evidence contexts and records diagnostic citation-aware exact-match scores, but its live run remains unperformed. The citation check establishes coordinate containment, not semantic entailment of every answer clause.

Three primary studies refine what to test next:

| Source inspected | Applicable lesson | Limit on inference |
|---|---|---|
| [LongMemEval v2](https://arxiv.org/html/2410.10813v2), 2025-03-04, especially §§3.3, 5.2–5.5 | Evaluate indexing, retrieval, and reading separately. Preserve original evidence while trying supplementary search keys. Treat memory updates, time reasoning, and abstention as distinct capabilities. | Its preferred granularity and budget vary by reader. Reported improvements are not oxibrain results, and summaries/facts can lose useful context. |
| [MemConflict v1](https://arxiv.org/html/2605.20926v1), 2026-05-20, especially §§3.1, 4.5.2 | Separate temporal updates, factual contradictions, and conditional applicability. Record whether an error came from missing evidence or failure to use evidence already retrieved. | This is a generated benchmark with a uniquely determined answer by construction. Its system comparisons do not transfer to this local deployment. |
| [TANGLE v1](https://arxiv.org/html/2608.13921v1), 2026-08-14, especially §§3.3–3.4, 4.1–4.3 | Include cases with no uniquely justified answer; preserve competing evidence and test conditional answers or targeted clarification. Compare complete-evidence and pipeline tracks. | The benchmark is synthetic and uses rubric/model judging with sampled human checks. It motivates diagnostic cases rather than a claim that one new policy will improve oxibrain. |

**Proposed evaluation.** Freeze a small independently annotated project-task set and external-memory subset before tuning. Include heading/body separation, renamed terms, multiple required sources, later corrections, version-specific instructions, contradictory authorities, unanswered questions, and genuinely underdetermined requests. Keep source visibility and T/K cutoffs identical across arms. Pin reader, tokenizer, embedder, prompts, and budgets. A 20-case smoke run of the existing four-arm/two-budget design is 160 reader calls; the full 200-case design is 1,600, plus the extraction fixtures. Choose and configure the provider explicitly before such a run.

Report full supporting-span coverage, supported answer correctness, unsupported assertions, appropriate abstention/clarification, citation faithfulness, latency, indexing cost, and paired uncertainty by independent trajectory. Review paraphrased correct answers and conflicting evidence manually; exact string matching is only a diagnostic. For abstention, retain no-evidence controls even when gold-evidence support is complete on positive questions.

## Suggested implementation sequence

1. Add desired-behavior regressions for heading expansion and scope-before-top-k; fix those bounded retrieval defects first.
2. Expose channel-specific execution/coverage and run a small live reader baseline with the existing runner and frozen diagnostics.
3. Add optional semantic source candidates as a measured ranking change, preserving lexical controls and PDC source identities.
4. Address truth-input selection and repeated tokenization with explicit scale measurements and invariant tests.
5. Expand conflict/condition evaluation before learned ranking, automatic reinforcement, or new experience schemas.

This sequence is a recommendation. It does not amend the roadmap or approve new formats, predicates, model downloads, or provider spending. The immutable ledger, complete temporal folds, reversible resolution, uncertainty, sixteen-tool limit, and read-only PDC boundary remain the constraints.

## Reproduction

Use a clean checkout of `40482ee`, with Rust 1.96.1 and the existing pinned tokenizer from the manifest. Copy `eval/reliable-recall/followup_probe.rs` from this research artifact into `crates/oxibrain-cli/tests/research_followup_probe.rs` in that disposable checkout. Set absolute, task-specific paths for `OXIBRAIN_TOKENIZER_JSON`, `OXIBRAIN_RESEARCH_OUTPUT`, and optionally an isolated `CARGO_TARGET_DIR`, then run:

```sh
cargo +1.96.1 test --locked -p oxibrain-cli --test research_followup_probe -- --nocapture --test-threads=1
cargo +1.96.1 test --locked -p oxibrain --test reliable_recall --test reproject_determinism --test parity_corpus
```

The first command writes one JSON observation per probe. Temporary store paths and timestamps vary; the reported retrieval outcomes and pure operation counts are the relevant comparisons. Remove the copied integration-test file before running unrelated workspace suites: it deliberately requires the two research environment variables. Keep the harness outside CI until desired-behavior acceptance cases are designed. The exact-token artifact digest and source revision are included in the combined observations.
