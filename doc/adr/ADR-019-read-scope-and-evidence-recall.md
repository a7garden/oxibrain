# ADR-019 — One read scope and source evidence for recall

Date: 2026-10-23. Status: accepted for implementation by the owner request; implemented in workspace 0.20.0, live quality gate open. The owner requested a design following the [recall study](../research/2026-10-23-recall-quality-and-evidence.md). Specification: [reliable-recall-v1](../spec/reliable-recall-v1.md).

## Context

The public operation path can find a document in search while returning empty recall. Relevant sessions disappear behind a newest-five limit or a 200-character prefix, a large trace is rejected whole, and search accepts `known_at` without applying it. A pure ranker's conservation property cannot cover source selection and packing decisions outside that ranker. Multiple facade/transport paths currently reconstruct different views of the same memory.

The existing immutable ledger, temporal fold, source occurrence identity and PDC reader boundary provide the necessary authority. The missing contract is how one scoped observation becomes bounded, cited consumer context.

## Design decisions

1. **One scope and visibility implementation serves search and recall.** Canonical request types carry both time axes. Core reconstructs visible claims from assertion inputs; every renderer, including snippets and profile, consumes that result. Legacy Rust methods delegate rather than reimplement. Current document caches do not supply historical semantics: explicit temporal filters with Documents are rejected with a typed, explanatory error.
2. **The selectable object is a source evidence unit.** Units bind source occurrence/identity, revision/view, UTF-8 byte ranges, evidence role, retrieval evidence and support. They are response/ranking data, never a new memory plane or user-document format. A quoted observation stays distinct from a folded claim. PDC UUID identity and legacy readability are preserved.
3. **Pure packing selects complete render variants.** Source-sized candidates retain their relevance, summary support and uncertainty travel together, and an oversized candidate cannot prevent consideration of later units. Selection and loss accounting conserve the considered unit set. Runtime output does not assert that it found all necessary evidence.
4. **Exact budgets name their tokenizer and rendering.** The canonical serialization includes source labels, references and separators. Tokenizer binding is independent from extraction. A caller can require exact accounting; the existing estimate remains an explicitly labeled compatibility path. Context is not duplicated in the response.
5. **Only rebuildable ranking indexes are added.** The schema-15 migration adds chunk lexical indexes and resumable backfill metadata. Sessions become searchable directly from their Primary episodes without extraction. Redaction, reprojection and backup omission cover the new indexes. No read writes a fact, reward or outcome.

## Consequences

- Sixteen MCP tools remain sufficient. Non-temporal old payloads and legacy Rust request structs retain compatibility; previously unenforced temporal/document combinations become explicit errors.
- Document schema 3 can supply revision-bound decoded-view spans. Decoder offsets are labeled correctly rather than confused with original HTML/Djot byte positions.
- Public truth filtering requires reconstructing competing assertions at the cutoff, rather than filtering today's winning belief. This costs more reads and needs explicit boundedness/latency measurement.
- Metadata becomes evidence about execution: index gaps, missing capabilities, filters, source races and budget loss are distinguishable. It is not a confidence score for the final answer.
- A new ranking schema needs a previous-version migration test and a verified isolated-restore rollback path. It does not justify source conversion or re-extraction.
- Dense retrieval and model reranking remain optional quality mechanisms. Their ports operate outside database transactions and cannot affect the truth fold.

## Alternatives considered

Raising recency limits or budgets alone leaves older sessions, late evidence and unsupported temporal scopes dependent on corpus size. An additional recall tool would split semantics and exceed the surface budget. A second durable session/experience store would duplicate the ledger's authority. Directly comparing cross-plane scores would assign meaning to incompatible scales. Learned memory writing or graph neural reranking can be evaluated after the evidence delivery baseline is correct and measured.

## Validation

The spec defines public CLI/MCP probes, temporal replacement/retraction cases, source/UTF-8 coverage properties, exact-token and loss conservation checks, redaction races, schema-14 migration and repeated reprojection tests. Quality evidence comes from a held-out in-house span corpus and fixed-reader comparisons. The [implementation report](../research/2026-10-23-reliable-recall-implementation.md) records actual checks, costs and remaining unrun experiments; source delivery is not an answer-quality claim.
