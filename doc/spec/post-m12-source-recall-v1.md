# Post-M12 source recall

Date: 2026-10-24. Architecture v2.26 / ADR-020.

## Read and index behavior

`admin index --memory --embed` rebuilds missing raw primary/session-window
vectors without extraction. Document indexing keeps `--documents --embed`.
Both are explicit model operations. These source channels embed only their query
text during reads; missing vectors remain a reported gap. The separate structured
claim channel retains its existing bounded on-demand embedding and reports that
work independently. No ordinary read downloads or indexes a model.

`EmbeddingPort::identity` binds weights and input configuration. Local GGUF
embeddings bind the verified weight digest, dimension, context and prefix hash.
A port without stable identity receives a runtime identity shared only by clones
of its Brain handle. Reattach after changing a port's configuration. Changing
weights does not reuse vectors just because dimensions match.

Memory schema 16 adds `memory_source_vectors`: chunk identity, revision, window
policy, model binding, dimension and little-endian float32 bytes. This projection
is disposable. Source windows refer to immutable ledger text, including readable
compacted bytes. Delete/redact/rebuild cascades invalidate vectors. Upserts check
source revision/liveness after inference. Reprojection with an embedder rebuilds
vectors; without one it leaves an observable pending index.

Source KNN eligibility applies space, ingestion cutoff, explicit observation
cutoff, current redaction and session exclusion before candidate limits. A
bounded batch scan scores in core and retains top-k. The default allowance is
16,384 vectors; reaching it reports `capped` and partial execution. The system
never claims the exact number of unseen relevant matches.

Document recall uses the hybrid channel when source semantics are enabled.
Lexical source channels remain independent and receive their own candidates.
`ReadPolicy.semantic_sources=false` supplies the lexical control. This policy is
an adapter-composition choice, never a truth-confidence setting.

## Receipts

`ReadMeta.channels` names each plane/channel and reports:

- eligible/executed, independently;
- model identity and indexed/total counts when known;
- vectors/candidates considered;
- known scan truncation (`capped`) separately from merely reaching a search
  return limit (`limit_reached`);
- fallback reason on optional-adapter failure or an unavailable model index.

Unknown counts are null, not zero. `dense_configured` remains a compatibility
capability flag; consumers use channel receipts for actual execution. An
incomplete configured semantic index or failed adapter makes execution partial.
The response never infers answerability from retrieval success.

## Complete truth groups

The store pages subject/predicate observations with bounded seed text visible at
K. Core keeps a bounded query-relevant seed set, prioritizing exact terms and
then character n-gram similarity. Only then does the truth byte guard select
complete groups. It does not take a matching assertion or current winner alone.
The facade materializes all competing values/denials/retractions for those groups
at K and folds in core. Source and identity observations are limited to selected
groups and their visible merge closure. Historical names cannot come from future
aliases or current cached beliefs.

The default seed-set allowance is 256 groups. Every catalog group is examined;
seed-text truncation, omitted groups, materialized groups, the approximate
field-size byte guard (not an RSS bound), materialized source count, selection time and fold time are reported in
`truth_selection`. Metadata scanning remains linear in group count and is
measured explicitly; no unmeasured cache or sublinear latency claim is made.

## Exact packing

Keep the accepted rendered context/count between attempts. Reuse token counts
for unchanged layers. A single-layer canonical context is the identical string,
so its count can be reused; multiple layers require counting the final joined
string. Per-unit token counts are never summed. The frozen reference packer is
test-only and checks byte-equivalent context, citations and decisions while
measuring actual tokenizer calls/bytes.

## Evaluation

`admin eval --suite reader` runs only the configured live reader experiment;
`--suite recall` runs span/semantic controls without a reader. Both use temporary
stores and sources. Live execution requires `live_reader=true`, an explicitly
chosen provider and pinned model identity. The config can select local embedding
weights, budgets, maximum cases and response-token cap.

The frozen follow-up corpus separates correction, conflict, condition, absent
answer, unspecified environment, version applicability, renamed entities,
multiple sources, temporal reasoning and exact identifiers. Four reader arms
remain retrieved / legacy / gold / no evidence. Structured `action` distinguishes
answer, conditional answer, clarification and abstention. Required phrases and
exact match are diagnostics; citation containment is not semantic entailment.
Captures permit manual review of the actual evidence and answer. Authored smoke
cases and five paired multilingual semantic controls do not substitute for an
independently annotated external benchmark or close the broad M12 quality gate.
