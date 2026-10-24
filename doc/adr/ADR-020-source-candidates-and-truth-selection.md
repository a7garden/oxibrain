# ADR-020: Source candidates and complete truth-group selection

Date: 2026-10-24. Status: accepted for implementation at the owner request.

## Context

The post-M12 study found semantic adapters disconnected from raw source recall,
a query-independent truth budget, repeated canonical tokenization, and an open
reader-quality gate. The owner requested implementation of the remaining work.

## Decision

- Add a schema-16 disposable vector projection for raw primary/session windows.
  Bind vectors to source revision, `source-window/1`, model identity and dimensions.
  Model identity includes weights and input options. Anonymous adapters receive a
  runtime identity; their cached vectors cannot be reused by a different handle.
- Share one process-wide llama.cpp backend between inference and embedding
  adapters (`oxibrain-llama-runtime`); an external initialization conflict returns
  a typed model error instead of panicking.
- Build vectors only through explicit indexing/reprojection, outside transactions.
  Raw-source/document reads embed only the query, fuse independent lexical/dense candidates,
  and preserve lexical fallback. Scope, transaction/observation time, redaction,
  and session exclusion apply before dense candidate limits. Scan in bounded
  batches; exhaustion of a configured scan allowance is explicitly partial.
- Report channel-specific eligibility, execution, coverage, model binding, caps,
  and fallback. Configuration alone never implies completed semantic indexing.
- Store observes paged truth-group metadata and complete assertion groups. Core
  chooses a bounded query-relevant set before the truth byte limit. Complete
  competing values, denials and retractions remain fold input. Identity labels
  and live sources are fetched for the selected groups and visible merge closure,
  rather than for the entire space. No ranking score or vector enters the fold.
- Reuse the accepted canonical render/count between pack attempts and reuse token
  counts for unchanged layers. Count the final whole context using the actual tokenizer;
  tokenization is never treated as additive. Preserve output bytes and citations.
- Freeze conflict, correction, condition and underdetermination cases before
  evaluation. Keep four reader arms and independent retrieval/reader diagnostics;
  local real-model smoke results do not close the general live quality gate.

## Consequences

The immutable ledger, truth determinism, current-only document scope, PDC reader
boundary, uncertainty/source bundles and sixteen-tool cap remain unchanged.
Caches can be deleted/rebuilt without losing source information. No migration
converts documents or extracts generated session summaries into trusted facts.
