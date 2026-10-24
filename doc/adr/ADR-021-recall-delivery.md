# ADR-021: Recall delivery and reader boundaries

Date: 2026-10-24. Status: accepted for the owner-authorized release work.

## Context

The actual local evaluation found missing supporting document spans and reader
errors with complete evidence. Inspection identified full-question AND queries,
section expansion tied to question wording, and incorrect signed-int8 distance
calculations. The reference reader copied source headers instead of identities
and could omit conditions while returning a valid action label.

## Decision

- Compute quantized distances through one pure signed-integer primitive. Validate
  dimensions and finite inputs at the adapter boundary. Small fractional positive
  and negative model components must retain their rank and exact self-distance.
- Generate bounded query components in core; stores fetch independent lexical
  observations and counts, and core fuses them with stable identity ties. No
  stopword lists, stemming, script branches, or case-specific keys are added.
- Balance raw-source lexical expansion and dense full-question retrieval as two
  families. Apply candidate caps after family fusion, preserve lexical-only and
  dense-only ordering, and partition every distinct candidate into kept/dropped.
  Dense errors gain relative influence; external evaluation must measure quality.
- Prefer generic paragraph and line boundaries in raw-source segmentation, with
  UTF-8 windows for oversized lines and byte-for-byte complete source coverage.
  Use 1,024-byte windows with 128-byte overlap for oversized lines, under
  `source-paragraph-window/3`. Schema 18 invalidates only disposable raw chunks, FTS, vectors and cursors;
  explicit indexing rebuilds from unchanged episodes. No connector marker or
  language-specific parser defines segmentation.
- Accompany a source's first ranked query hit at a nonzero offset with one
  bounded prefix (default 768 bytes, zero disables). Preserve query-hit order;
  do not create companions from recency alone. Core selects episodes and UTF-8
  boundaries, store fetches ordinal-zero indexed bytes through every source
  scope filter, and facade sequences ordinary untrusted observations immediately
  after their query hit. Channel/materialization bounds and final exact packing
  apply. Receipts describe candidate bytes separately from delivered evidence.
- Drop a source range as duplicate only when a delivered range fully contains it
  with identical revision/view/decoder/UUID provenance. Partial overlaps retain
  their unique tails and consume actual rendered tokens.
- Expand a selected section's body/title hit using its structural section range,
  rather than requiring every word in a natural question in its title. Keep
  revision/view identity and complete applicability context.
- Preserve independently retrieved peer sections even when they match fewer
  query anchors. A strict subset of anchors does not prove that a section's
  condition or veto is redundant. Match strength orders evidence; budget losses
  stay explicit. Citation ranges remain section-bound even when another peer
  appears as separate evidence.
- Use compact evidence by default (`ReadPolicy.compact_evidence`, policy v5).
  `context_render/2` uses E1, E2, ... only within a response. `evidence` retains
  full source coordinates, revisions, views, support, omissions, and text ranges.
  IDs are never durable document or ledger identities. Full legacy rendering
  remains available through explicit `compact_evidence:false`; the legacy
  `pack_evidence()` convenience function also retains full framing. Both renderings count complete canonical
  context strings with the configured tokenizer.
- Define a pure consumer-side reader envelope with quoted evidence, verbatim
  conditions, an answer action, answer, and clarification. Validate citation IDs,
  delivered quotation containment, quoted conditions and structural consistency.
  A clarification cannot also issue an answer. Rejected output is an explicit
  limitation, never silently accepted or repaired into a factual claim.
- Keep this validation distinct from entailment, completeness and answerability.
  The engine remains a memory library, and recall still reports
  `answerability=not_evaluated`. No model output becomes truth or a new trusted
  ledger assertion. The reference reader is an evaluation client of this API.

## Verification

Known cases are regression diagnostics. Freeze external evaluation inputs and
separate gold annotations before final candidate-policy evaluation. Report exact
retrieval metrics, delivery validation and manual semantic findings separately.
No containment score is renamed semantic correctness. The shipped skill is
forward-tested independently and release artifacts are verified before promotion.
