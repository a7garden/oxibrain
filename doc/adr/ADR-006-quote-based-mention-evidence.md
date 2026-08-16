# ADR-006: Quote-Based Mention Evidence

- **Status:** Accepted
- **Date:** 2026-08-16
- **Supersedes:** none
- **Superseded by:** (none yet)

## Context

The extraction contract (§9.4) requires the model to emit, per entity mention, a byte
span `[start, end)` at which the surface must appear verbatim. This is the
fabricated-entity gate: spans are provenance citing the exact bytes that support a claim.

Measured 2026-08-16 on the default local extractor (qwen2.5-1.5b-instruct, GBNF,
Metal): **4/4 claims quarantined** on two real freeform notes (one Korean, one English).
Failure modes: `surface_not_verbatim` (spans pointing tens to hundreds of bytes away)
and `span_out_of_bounds` (an inverted `[679, 80]`). The repair ladder
(exact-byte → char-index → casing) exists for *interpretation drift*; these spans are
not drifted, they are hallucinated. Small language models copy text reliably but
cannot count offsets.

Extending the ladder with "search the surface and relocate the span" is forbidden by
design: `injection_suite::verbatim_surface_required` requires that a span citing the
wrong bytes is rejected even when the surface occurs elsewhere — relocation would
accept provenance the model never actually identified.

## Decision

The model stops providing numeric spans. Per mention (and per literal object) it
provides a `quote`: a short verbatim snippet copied from the episode containing the
surface. The server locates the quote (first occurrence, exact bytes), requires the
surface inside the located window (case-insensitive fallback, surface canonicalized to
the source), and derives the byte span server-side.

- Grammar/schema no longer contain `span`; the validator's stored spans become
  server-computed and byte-exact.
- Legacy responses without `quote` keep the old ladder (cached extractions, eval
  fixtures, HTTP tiers).
- `prompt_version` 1 → 2: new ExtractorId, so old extraction caches are never silently
  mixed with the new contract (§9.5).
- Few-shot selection (10.8) is wired into the facade prompt from a small built-in
  multilingual corpus, chosen by character-trigram Jaccard (P11).

## Consequences

- The fabricated-entity gate is preserved: a fabricated surface has no copyable quote;
  a quote that does not contain its surface is rejected outright.
- Instruction-shaped text that genuinely occurs in an episode (e.g. inside a code
  block) remains data, exactly as `injection_in_code_block_rejected` variant B
  documents.
- Multi-occurrence surfaces are disambiguated by the quote, which the numeric-span
  contract could not express for a model that cannot count.
- Output cost grows by the quote tokens (~20–40 per claim) — accepted.
- ARCHITECTURE.md §7.4/§9.4 updated in the same change (docs commit, version bump).
