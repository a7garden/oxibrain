# ADR-023: Document memory retrieval quality

Status: accepted for implementation; release acceptance requires the checks below.

Date: 2026-11-02

## Problem

Operational project-memory probes found three different limits. The host adapter
used lexical document search only, reused evidence across unrelated short
questions, and delivered prefix excerpts that could omit a relevant passage.
Document hybrid retrieval also treated the entire vector index size as the
dense channel's lexical breadth. Growing an unrelated corpus therefore weakened
semantic candidates even when their semantic order did not change. Finally, a
global bounded reconciliation pass could spend its budget before reaching the
space requested by a search.

These are retrieval and delivery problems. They do not authorize rewriting
source documents, inventing a newer truth, or retracting immutable assertions.
In particular, a `requires` predicate with coexist semantics does not become
single-valued merely because an operator has a newer prose decision.

## Decision

1. Under `read-policy/6`, fuse bounded lexical query channels within their
   existing specificity-aware family. Fuse that ranked family with the ordered
   semantic family using equal reciprocal-rank weight, then apply the final
   candidate cap once. An unrelated vector count is coverage metadata, not a
   cross-family relevance penalty. Lexical-only and dense-only orders remain
   compatible; omitted candidates remain accounted for. Equal family scores
   prefer lexical-family membership and rank before source identity.
2. Bounded search/recall reconciliation prioritizes configured roots belonging
   to the requested space. Keep the complete root-set diff, full admin-index
   behavior, existing source/cache guards, and honest skipped-root receipts.
   Do not derive `RemoveRoot` actions from a filtered configuration subset.
3. The host integration may request a bounded hybrid document candidate pool,
   diversify delivered sources, remove repeated ranges only under matching
   source identity/revision/view, and choose query-centered UTF-8 excerpts.
   Every delivered byte range must still identify the exact original source
   slice. Explicit recall remains fresh; a cache hit requires the same query,
   independent of language or writing system. Neighbors remain unverified
   candidates. The host caps snippets without query-feature overlap at three,
   prioritizes snippets with overlap, and labels exclusively no-overlap output
   as candidate evidence. Feature overlap is neither per-hit channel provenance
   nor proof of semantic relevance; ordinary shared fragments can pass it.
   Full local retrieval receipts have bounded retention.

The Rust engine remains independent of the Python host adapter. No new model,
service, public operation, document format, ledger schema, or source-writing
feature is introduced. Failed or unavailable dense retrieval remains visible in
execution receipts and can fall back to lexical candidates. Ranking policy never
changes the temporal fold or source trust.

## Acceptance and limits

- A deterministic semantic paraphrase regression must fail before the family
  fix and pass after unrelated index growth; dense failure must retain lexical
  results with explicit fallback evidence.
- Pure fusion checks cover determinism, duplicates, final caps and conservation.
- An unrelated large root must not starve requested-space freshness, and its
  existing cache and source bytes must survive.
- Host tests cover cross-topic cache isolation, explicit refresh, multibyte
  excerpt coordinates, repeated versus independent provenance, source diversity,
  budget bounds, and existing record/retry behavior.
- Compare actual operational queries before and after installation and publish
  failures as well as improvements. These probes are not a general accuracy
  benchmark and do not close the M12 reader-quality gate.

Historical records remain historical. Current operational state still needs a
current authoritative observation; retrieval ranking does not prove that an old
decision is still in force. Graph assertion reconciliation requires supported
validity/denial declarations or a separately evaluated extraction change.

The patch release retains schema 19. Deployment keeps the previous compatible
binary, host adapter/configuration and a verified store backup for rollback.
