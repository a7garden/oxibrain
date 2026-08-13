# ADR-004: embedding_sim returns 0.0 during projection

- **Status:** Accepted
- **Date:** 2026-08-13
- **Supersedes:** none
- **Superseded by:** (none yet)

## Context

Resolution (§10) combines three signals into a per-candidate decision:

1. **n-gram / LSH block score** — the MinHash/LSH candidate set already gates
   what gets compared, so the per-candidate score is mostly `1 - jaccard`.
2. **Graph-context overlap** (§10.2) — Jaccard between the candidate's
   neighbour set and the mention's context entities. Non-zero when a
   co-occurring entity shares neighbours.
3. **Embedding similarity** (§10.3) — cosine over dense entity vectors.
   Hypothetically the strongest signal for short / typo'd / transliterated
   surface forms where n-gram and graph context are both weak.

Embeddings are computed *post-projection* by `embed_entities`, which is run
after the projection phase by `reproject` and the `Brain::reproject` /
`Brain::ingest` paths. During projection itself, the entity vectors table
is either empty (first ingest) or stale (incremental ingest, where a new
entity was just created and not yet embedded). Reading a stale or empty
vector would give `embedding_sim = NaN` or 0.0 in a way that would
silently degrade resolution quality.

The wiring decision is in `project.rs::embedding_sim` (a `&Connection → f64`
closure passed into `resolution::resolve`). The function body is:

```rust
fn embedding_sim(_conn: &Connection, _candidate: &str) -> f64 {
    0.0
}
```

This was flagged in the 2026-08-12 handoff as a known gap.

## Decision

**Leave `embedding_sim` at 0.0 during projection.** Document why; do not
silently substitute a stale or empty vector. PerType weights
(Person/Org 0.1, Concept 0.6, default 0.3) are set in
`ResolutionConfig` but inert until the architectural decision changes.

The trade-off:

- **Pro (keep at 0.0):** projection stays deterministic and side-effect
  free, matching P1 (byte-identical reproject). Resolution depends only
  on n-gram + graph context, both of which are read from the ledger
  state. A failed/empty embedder never poisons resolution.
- **Con (keep at 0.0):** resolution quality on short / typo'd surfaces
  is lower than it could be. The §5.1 ranking-tolerance measurement
  (2pp floor) was made with embedding_sim = 0, so any improvement from
  enabling embeddings will be unaccounted for until re-measured.

## Alternative considered: project-time embedding

`embed_entities` could be moved into the projection path so the vector is
written before the next mention of the same type is resolved. This
requires:

1. Opening a transaction, doing the LLM-or-local embedding, writing the
   vector, then doing the resolution — which violates §18 rule 1
   ("no transaction across an inference call").
2. OR: an eager-embed pass after the ledger write but before the
   resolution pass. Two-phase projection. Cost: one extra writer round
   trip per `Brain::declare` (reproject amortizes this).

The "no transaction across inference" rule is load-bearing for the
offline / batch reproject case (which has no live embedder). The cleanest
path forward is the two-phase approach, but it has not been measured
against the alternative of just letting `embed_entities` run as today
and accepting the n-gram + graph signal only at projection time.

## Re-evaluation trigger

Re-open this ADR when any of the following becomes true:

1. The §17.2 three-arm gate shows (c) − (b) is small on
   `temporal_reasoning` with a strong extractor (the D19 demote
   pre-commitment).
2. A measurement on a 10⁴-entity fixture shows resolution is failing
   on the n-gram signal alone for short / typo'd surfaces, and the
   fix is non-architectural (e.g. extending the LSH band count, which
   the §5.1 tolerance study does not rule out).
3. A new product feature explicitly requires resolution over fuzzy
   surfaces (e.g. "find this person even if the name is misspelled")
   and we cannot punt the fuzzy match to retrieval-time semantic
   search.

Until one of those triggers fires, the projection path is unchanged
and `embedding_sim = 0.0` is the documented contract.
