# Quote-Based Mention Evidence — Extraction Contract v2

**Date:** 2026-08-16 · **Status:** approved (user delegated autonomous progression) · **ADR:** doc/adr/ADR-006-quote-based-mention-evidence.md

## Problem (measured)

Live smoke 2026-08-16: the local extractor (qwen2.5-1.5b-instruct, GBNF-constrained,
Metal) quarantined **4/4 claims** on two real freeform notes — one Korean, one English.
`extraction_failures` shows the root cause is span arithmetic, not language:

- `surface_not_verbatim` — "Carol" cited at [231,245]; content is 275 bytes, "Carol" lives at ~57.
- `span_out_of_bounds` — [679, 80]: an inverted, hallucinated span.

The existing repair ladder (exact byte → char-index → casing drift) fixes
*interpretation drift* of approximately-correct offsets. These offsets are not drifted;
they are fabricated. Small models cannot count bytes, and the injection suite
(correctly) forbids relocating a wrong span to another occurrence — so the ladder
cannot be extended with search-and-relocate without weakening the fabricated-entity gate.
Golden-corpus fixtures pass only because their sentences are short and the cached
responses were produced when they passed.

## Decision

**Stop asking the model for arithmetic. Ask it for copied evidence instead.**

Per mention (and per literal object), the model emits a `quote`: a short verbatim
snippet copied from the episode that contains the surface. Copying is a language-model
strength; counting is not. The server deterministically locates the quote and derives
the byte span:

1. `content.find(quote)` — first occurrence, exact bytes. Not found → reject.
2. Within the quote's window, `find(surface)` — first occurrence, exact; on miss,
   case-insensitive fallback (surface canonicalized to source text, as ladder step 3).
   Not found → reject.
3. `span = (quote_start + off, quote_start + off + surface.len())` — byte-exact,
   server-computed.

A quote that does not contain its surface is invalid even if the quote itself is found —
this closes the degenerate "empty quote = surface anywhere" hole and keeps the quote
meaningful evidence.

### Why every gate survives

- **Fabricated-entity gate (§7.4):** a fabricated surface has no verbatim quote to copy;
  the quote lookup fails and the claim is quarantined. Instruction-shaped surfaces that
  genuinely occur in episode text are accepted as *data* — identical to the documented
  stance of `injection_suite::injection_in_code_block_rejected` variant B.
- **No relocation of model-provided spans:** the model no longer provides spans. The
  *evidence* (quote) is located, not a guessed offset. `verbatim_surface_required`
  (Alice@(16,21), no quote) still rejects via the legacy ladder, unchanged.
- **Determinism (P1):** pure functions; first-occurrence rules are total orders.
- **Provenance:** stored spans remain byte-exact — now computed by the server, not
  hallucinated by the model.

### Contract changes

| Site | Change |
|---|---|
| `MentionRef` | + `quote: Option<String>` (serde default); `span` gets serde default (derived when quote present) |
| `ClaimObject::Literal` | + `quote: Option<String>`; span derived likewise — strictly stronger than today's bounds-only check |
| `resolve_mention` ladder | new step 0: quote-locate; steps 1–3 unchanged for legacy responses |
| `grammar_from_registry` | `mention ::= surface, entity_type, quote`; literal drops `span`, gains `quote` |
| `schema_from_registry` | same shape as grammar; `span` removed from model-facing schema |
| `build_extraction_prompt` | v2 instructions (copy-the-quote); few-shot injected by the facade via `format_few_shot` |
| `ExtractorConfig::prompt_version` | 1 → 2 everywhere ⇒ new ExtractorId ⇒ old caches untouched (§9.5 cache discipline) |

Backward compatibility: responses without `quote` (cached extractions, eval fixtures,
HTTP providers mid-migration) take the legacy ladder. `eval fast` must stay green
unchanged — it proves the legacy tier does not regress.

### Few-shot (10.8 wiring)

`format_few_shot`/`few_shot_examples` exist but were never called. The facade composes
`system = build_extraction_prompt(predicates) + format_few_shot(selected)` from a small
built-in multilingual corpus (one English person-facts example, one Korean example, one
literal-value example), selected by character-trigram Jaccard (P11: language-independent
by construction). Selection is pure and deterministic.

## Testing

- Core unit: quote happy path (en + ko), quote-not-found reject, surface-not-in-quote
  reject, multi-occurrence disambiguation (quote picks the right "Alice"), casing
  tolerance in-window, literal quote derivation, legacy-ladder regression.
- `injection_suite` additions: fabricated quote rejected; code-block quote containing an
  injected surface remains data (variant-B parity).
- `eval fast`: unchanged fixtures pass (legacy path).
- Live acceptance: the two smoke notes (Korean + English) yield ≥1 valid statement each
  where they previously yielded zero.

## Non-goals

- No relocation/search of model-provided numeric spans (injection suite forbids it).
- No predicate-registry changes; no model-weights change.
- HTTP-provider prompt regeneration beyond the shared schema/prompt functions.
