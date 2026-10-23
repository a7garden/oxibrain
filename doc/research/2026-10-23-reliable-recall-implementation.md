# Reliable recall implementation and verification

Date: 2026-10-23. Baseline: `1a14f2d` / workspace 0.19.0 / ledger schema 14. Implementation: working tree 0.20.0 / ledger schema 15 / document cache schema 3. The owner requested implementation of the researched design. No commit, merge, installed-binary replacement or production-store migration was performed.

## Implemented behavior

Public `search` and `recall` use `ReadScope` and a shared reconstruction of assertion groups at valid time T and knowledge time K. Names come from visible verbatim mentions and merge declarations at K; current merge redirects cannot enrich a historical read. Profile, claim snippets and one-hop graph expansion consume those scoped claims. Explicit document time filters return `unsupported_temporal_scope`; current document caches do not pretend to be acquisition-time ledgers. Optional dense claim inference runs after the DB fetch has released its handle, validates its output, and records a lexical fallback on failure. The legacy lower-level `Brain::query` executor is retained; public scoped search no longer uses its current-belief enrichment.

Primary raw sources, including sessions and compacted bodies, are indexed atomically on ingest into separate `memory_source_chunks` plus contentless word/trigram FTS. Windows cover complete UTF-8 input, including separator-free tails; they do not overwrite the legacy contextual chunk table. Source occurrence identity remains authoritative. `admin index --memory` captures a frontier and commits each canonical batch with its versioned cursor. It repairs omitted projections without extraction, and reprojection rebuilds the ranking state. Redaction and detached-backup omission cover the new tables.

Pure evidence packing interleaves source/summary/claim channels and alternates query-bearing planes, retains structured claims and summary support/uncertainty, tries smaller source quotations, continues after oversized candidates, and records each considered unit's terminal decision. Quotations carry exact source ranges, window omissions, role and revision. Documents add UUID, decoder identity, decoded-view digest and root generation. Chunk ranges, UUIDs and root generations are fetched in one short SQLite snapshot before filesystem materialization. Current source revisions are checked again before return. A changed exposure epoch or selected document revision causes one fresh retry, then `Busy` if it changes again. Background topic, runbook and community synthesis similarly validates its captured exposure state and live non-derived support before publishing cache/derived/checkpoint results.

`context_render/1` is exactly the layer texts joined with two newlines. Source labels, UUIDs, references and separators are inside the counted text. JSON receipts/explanations are outside that prompt budget and independently bounded. `FileTokenizer` binds an existing `tokenizer.json` with a BLAKE3 artifact ID without loading weights or downloading anything. Strict requests fail with `tokenizer_unavailable` if an exact tokenizer is absent; the compatibility character fallback is explicitly labeled as an estimate. `oxibrain_client::render_recall_context` renders the JSON form. CLI results retain the existing outer envelope (`data.data` for the recall DTO, `data.meta` for its receipt).

`ReadPolicy` version 1 exposes engineering limits through a facade builder, explicit CLI configuration and MCP experimental capabilities. Document reconciliation has a 4,096-entry / 8-MiB default read bound and never applies an incomplete root diff. Document materialization is bounded; omitted/stale considered hits are reported. Complete truth groups have a separate 8-MiB preflight bound and are excluded whole if they do not fit. Candidate windows and sidecar metadata have their own limits. These are engineering limits, not calibrated relevance thresholds or proof of corpus-wide completeness.

## Correctness and regression evidence

- Full workspace tests with all features: **1,044 passed, 0 failed, 5 existing ignored**, including doc tests. Additional snapshot, exact-count-failure and merge-visibility regressions are included.
- Property coverage: full UTF-8 window coverage/progress, quotation range fidelity, deterministic packing/IDs, nonadditive canonical token counting, considered-unit conservation, and continued selection after an oversized first unit.
- Historical functional replacement, denial/retraction and independently varying T/K are exercised. A future alias/merge does not enrich an earlier read. Historical summaries without verifiable scope are omitted.
- Actual pauses during model/embedding work exercise redaction races. Stale synthesis cannot publish a summary/cache entry. Recall retries after redaction and returns no deleted claim. A document edit during packing returns the new verified revision rather than the old bytes.
- Schema 14→15 snapshot test preserves ledger/truth and performs no extraction; migration-chain tests pass. Index rollback leaves both rows and cursor unchanged; repeated indexing, compacted sources, projection omission and reprojection retain coverage. Existing truth determinism and writing-system parity tests remain enabled and pass.
- Canonical PDC Markdown reaches recall through both CLI and MCP with its UUID and revision, and source bytes remain unchanged. Existing PDC/legacy/document integration tests pass.
- Additional real-tokenizer public CLI and stdio MCP probes passed for a relevant session behind ten newer sessions, evidence beyond the old prefix, an exactly 48,735-character trace tail and a historical knowledge cutoff. At a 2,000-token budget the matching CLI/MCP contexts spent 1,021 / 936 / 398 tokens respectively. See `eval/reliable-recall/results/public-probes.json`.
- `fast` fixture replay: precision 1.000, recall 1.000, fabricated entity rate 0.000. This is canned extraction, not a live-model result. `experiential`: summary 2/4, sources 4/4, hybrid 4/4, no-memory 0/4; the existing exit condition still holds.
- The legacy gate harness now pins its actual request time with manifest `read_at` and passes each temporal question's `as_of` to RecallRequest. Its previous numeric clock was 2023 despite a 2024 comment, and the recall arm omitted the question cutoff. With corrected scope, recall reaches 20/20 knowledge-update and 13/13 temporal answers; the graph category has one remaining miss (8/9). This is fixture string reachability, not live QA. The existing lexical/hybrid graph control remains separately labeled.
- Correcting the experience clock exposed source starvation: hybrid temporarily fell to 3/4 while sources-only stayed 4/4. The pure packer now interleaves source, summary and claim channels before alternating planes. The regression is fixed: experiential hybrid is again 4/4; the broader experience fixture reaches workflow 3/3, gotcha 3/4, premise 4/4, static 2/2 and dynamic 1/1. The remaining gotcha miss is reported rather than hidden.
- Required build, formatting, clippy, dependency/advisory and standalone checks are recorded separately from the synthetic source-support measurement. Two transitive certificate dependencies were patched to satisfy current advisories; no advisory suppression was added.

## Synthetic source-support measurement

Check receipts: [`implementation-checks.json`](../../eval/reliable-recall/results/implementation-checks.json).

Artifacts: [`eval/reliable-recall/manifest.json`](../../eval/reliable-recall/manifest.json), [`results/source-support.json`](../../eval/reliable-recall/results/source-support.json), and the [runner instructions](../../eval/reliable-recall/README.md).

The frozen verification partition has 200 questions: 160 with annotated required source spans and 40 with no supporting answer. It covers documents, old session tails, long traces and two-source prerequisites, with Latin, Hangul, CJK, RTL and combining-character classes. Required spans bind the correct relation and source; identical answer text in a rejected staging example does not receive credit.

The actual tokenizer was Qwen/Qwen3-0.6B `tokenizer.json`, pinned at repository revision `c1899de289a04d12100db370d81485cdf75e47ca`. Its BLAKE3 is `b0cb923fc505fdf0a53f0287654fa26577d3f333d4134350da0a97664b228739`. No model weights or live inference were used. The runner independently recounted every emitted context and verified original PDC file hashes after evaluation.

| Budget | Full Support | Recovered necessary span bytes | Read p50 | Read p95 |
|---|---:|---:|---:|---:|
| 2,000 | 160/160 | 100% | 506 ms | 1,085 ms |
| 3,000 | 160/160 | 100% | 693 ms | 1,467 ms |

Every positive category and each writing-system class recovered all required spans in this fixture set. No context exceeded its actual tokenizer budget. The legacy context arm recovered 0/160 full supports under each budget; its provenance credit requires complete verbatim gold spans and matching layer provenance because it has no range receipts. This is a targeted reproduction comparison, not a general retrieval leaderboard.

The corpus contains 2,338,680 source bytes. Store plus index files occupied approximately 8.2 MB. Initial ingest/indexing took about 1.35 seconds. The complete two-budget run took 330.83 seconds; external `/usr/bin/time -l` measured maximum RSS 177,422,336 bytes and peak memory footprint 124,371,592 bytes. Builds/tests on the same machine may affect latency; these are indicative costs, not isolated performance measurements. Per-question latency, context bytes, tokens and trajectory-grouped paired intervals are in the JSON artifact.

The initial diagnostic partition exposed an invalid document-space fixture and a genuine common-term fusion failure. Both are preserved in `results/development-diagnostic.json`, explicitly invalid for quality comparison. The fixture now uses the connector's space name. Bounded source channels use inverse observed channel breadth before RRF so several broad common terms cannot overwhelm a narrow exact-source channel. A dedicated regression covers this case; the weighting is not corpus IDF or a learned policy.

Verification and development have disjoint trajectory labels and answer vocabulary but share templates. Annotations were generated programmatically. The verification set is synthetic and does not establish real-user generalization or adequate statistical power. The 40 no-answer cases do not grade reader abstention in offline mode. Correct-answer-with-provenance, unsupported answers and appropriate abstention remain unmeasured until a reader runs.

## Remaining experimental acceptance

The `full` stub is replaced by an explicit, pinned live reader/extraction runner. It records matched retrieved/legacy/gold/no-evidence requests and responses, citation-aware diagnostic answer scores, costs, and a replayable extraction artifact. The caller must supply evaluation configuration and an explicit provider. Its full 200-question configuration entails 1,600 reader calls plus the existing three extraction fixtures. No provider was configured or live run performed in this task.

Consequently the M12 live supported-answer quality gate remains open. A real lexical+dense matched control, external LongMemEval/LoCoMo, learned memory policies and new model-reranker policies remain follow-up experiments; no gain is inferred for them. Synthetic source delivery and implementation correctness are demonstrated, while answer accuracy, hallucination/abstention quality and live extraction generalization are not claimed.

## Use and rollback

Build the working tree binary. Existing schema-14 stores require the normal schema-15 upgrade on first writable open; take the existing detached backup before upgrading a production store. Raw backfill is explicit `admin index --memory`; ordinary reads report pending coverage and do not drain a backlog. `admin index` keeps documents as its default. Rebuildable ranking policy can be rolled back by rebuilding; an older schema-14 binary requires a verified pre-upgrade backup restored into a new directory, not an in-place downgrade. No user note authoring, conversion or bulk re-extraction was added.
