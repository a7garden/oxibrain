# PDC Adoption v2 — Markdown-first Reader and Indexer

Status: implemented (Full Reader/indexer path); corpus rev 2 pinned

External contracts: `pdc-document/2` (tag `v2.0.0-draft.2`, commit `0ee51ea`),
`pdc-query/1`, legacy `pdc-document/1` (frozen, draft 4)

Corpus pins: `pdc-document-conformance/2` revision 2
(`crates/oxibrain-connectors/tests/fixtures/pdc2-corpus/`, see its `PIN.md`)
and the frozen v1 corpus revision 3 (`tests/fixtures/pdc-corpus/`)

Target capability: Full Reader/indexer; never Writer or Mutator

This specification supersedes `pdc-adoption-v1.md` at the document connector
boundary. It does not change the immutable memory plane, document ownership,
or the rule that oxibrain never writes a user's vault.

## Role and invariants

1. Oximemo, Sawhorse, and other document owners author PDC files. Oxibrain
   only scans, validates, decodes, indexes, and serves source references.
2. Canonical inputs: `pdc-markdown/1` (lowercase `.md`, Obsidian-compatible)
   and `pdc-html/1` (HTML transport, both envelope majors). Legacy inputs:
   `pdc-document/1` documents (`.djot`, `pdc-djot/1`/`pdc-html/1` bodies) —
   readable, indexed under the frozen v1 rules, never auto-converted
   (PDC 2 §6.3, outcome `legacy_document_version`).
3. Plain Markdown (`.md` without valid PDC frontmatter) is visible legacy
   input (`legacy_markdown`); unmarked HTML stays `legacy_html`. Neither is
   malformed, and neither is a conversion target.
4. Envelope metadata never becomes an episode and never bypasses source
   trust or validation. Envelope text never reaches the index.
5. `documents.db` remains a disposable projection. Re-indexing from source
   reproduces its PDC metadata and body-derived index.
6. No connector code writes, repairs, normalizes, assigns IDs to, or
   migrates source documents. Query definitions are never executed.

## Envelope (pdc-document/2)

- Safe general YAML 1.2 Core under the contract restrictions: comments and
  nested JSON-compatible mappings/sequences allowed; anchors, aliases, tags,
  complex keys, multi-doc streams, non-string or duplicate keys, and
  non-finite values rejected; depth > 32 or > 10 000 nodes is
  `document_too_complex`.
- Required fields: `format` (exactly `pdc-document/2`), `body` (`pdc-markdown/1`
  or `pdc-html/1`, matching transport), `id` (canonical lowercase UUID),
  `created`, `updated` (canonical timestamps, `updated >= created`), `title`.
- Unknown top-level keys are user properties: legal, source-preserved
  verbatim via `doc://` provenance; the disposable projection records the
  standard fields only.
- The v1 envelope keeps its frozen constrained grammar; dispatch is by the
  observed `format` value before parsing.

## Identity

The path-derived `document_id` remains the internal cache/provenance key for
`doc://` behavior. `pdc_document_id` (the envelope UUID) is canonical for
`pdc://` resolution, unique per root (partial unique index `idx_documents_pdc_uuid`,
schema v3), and stable across moves and renames. Duplicate UUIDs across body
profiles and both majors are vault conflicts: the facade reports every
claimant and drops all but its deterministic survivor from the apply pass.

## Body projection (pdc-markdown/1)

The body text is the verbatim post-frontmatter source — the legacy Markdown
scan — so every construct stays indexed as inert text/source. The projection
adds: caret block IDs (`[A-Za-z0-9-]+`, plus `id="b-<uuid>"` in raw HTML) as
stable targets; GFM task items (other checkbox states are preserved
extensions); canonical `pdc://document/<uuid>[#b-<uuid>]` links and
`pdc://asset/sha256/<digest>` managed-asset references; wiki links, embeds,
and vault-relative Markdown links recorded as written (projection-only,
never resolved); active/unsafe constructs (`script`-family, refresh metas,
event handlers, `javascript:`/`vbscript:`/`data:` URLs) flagged
`unsafe_content` while the source stays inert and indexed.

## Query contract (pdc-query/1)

`.base` files and fenced `base` blocks are opaque, read-only query
definitions: validated against the same safe-YAML rules (malformed →
`invalid_query`, file stays visible and untouched), preserved verbatim,
indexed as source, counted as `query_definitions`. oxibrain has no query
execution engine; there is no scripts/network/process/authorization surface.

## Diagnostics

Contract vocabulary spelled exactly: `invalid_transport`, `invalid_envelope`,
`invalid_query`, `legacy_html`, `legacy_markdown`, `legacy_document_version`,
`unsupported_document_version`, `unsupported_body_version`,
`invalid_document_id`, `duplicate_document_id`, `duplicate_block_id`,
`document_too_large`, `document_too_complex`, `missing_asset`,
`asset_digest_mismatch`, `unsafe_content`, plus the oxibrain-only
`unresolved_link`. Counters surface through `DocumentFreshness` and the
`doctor`/`index` CLI output. Indexing one bad document never hides others.

## Non-goals

No PDC authoring, export, source repair, migration, asset garbage collection,
or vault management; no conversion of documents into episodes; no new MCP
tool solely for PDC; no query execution; no network fetch while indexing.

## Verification

```bash
cargo fmt --all -- --check
cargo test -p oxibrain-connectors            # unit + both corpus runners
cargo test -p oxibrain --test document_plane # facade classification/projection
cargo clippy --all-targets --all-features -- -D warnings
cargo build -p oxibrain --no-default-features --features http-llm
```

## Corpus resync requirement

The vendored copy is pinned to upstream commit `0ee51ea`. When the upstream
corpus moves, copy `corpus.json` + `fixtures/`, update `PIN.md`, and bump
`PDC2_CORPUS_REVISION` in `crates/oxibrain-connectors/src/pdc/mod.rs` in the
same change. Never hand-edit vendored fixtures to make a test pass.
