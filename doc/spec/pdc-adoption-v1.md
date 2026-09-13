# PDC Adoption v1 — Reader and Indexer

Status: priority 1 document-connector work, implementation pending  
External contract: `pdc-document/1`, corpus revision 3  
Target capability: Full Reader/indexer; never Writer or Mutator

This specification adds Portable Document Contract support at the document connector boundary. It does not change the immutable memory plane, document ownership, or the rule that oxibrain never writes a user's vault.

## Role and invariants

1. Oximemo, Sawhorse, and other document owners author PDC files. Oxibrain only scans, validates, decodes, indexes, and serves source references.
2. `.djot` and marked PDC `.html` are canonical inputs. Unmarked HTML remains visible legacy input under the existing adapter.
3. Envelope metadata never becomes an episode and never bypasses source trust or validation.
4. `documents.db` remains a disposable projection. Re-indexing from source reproduces its PDC metadata and body-derived index.
5. No connector code writes, repairs, normalizes, assigns IDs to, or migrates source documents.

## Identity model

Oxibrain's existing path-derived `document_id` remains an internal cache/provenance key for `doc://` behavior. It must not replace the canonical PDC UUID.

- Add nullable `pdc_document_id`, `pdc_body_profile`, and parsed standard metadata to the disposable document projection.
- Enforce uniqueness of `pdc_document_id` within one PDC vault/root and surface every conflicting locator.
- Resolve `pdc://document/<uuid>` with the canonical UUID mapping, never by path-derived cache ID.
- Preserve the existing locator and bytes-shaped revision for raw source provenance and Git history.
- A move may change the cache key/locator but not the PDC UUID; link resolution remains stable after re-index.

Any projection schema change still ships with the normal migration and up-test even though the store is rebuildable.

## Connector changes

### Discovery and classification

- Scan lowercase `.djot` and `.html` without following symlinks or entering excluded dot directories.
- Every `.djot` produces a document or explicit PDC diagnostic.
- Classify `.html` as canonical PDC HTML, visible legacy HTML, or invalid PDC transport.
- Honor canonical deleted metadata in trash/recovery filtering while retaining the source in discovery.

### Decode

- Add a shared constrained-envelope parser and transport dispatcher.
- `pdc-djot/1`: parse the pinned Djot profile, extract text and source ranges, and never leak envelope text into FTS.
- `pdc-html/1`: preserve raw source for `doc://`, extract text with the HTML path, record unsafe constructs, and never execute or fetch content.
- Extract standard title, tags, aliases, deletion state, block IDs, UUID links, managed-asset references, and task state into the document projection.
- Preserve unknown envelope data as opaque metadata when useful for diagnostics; never reinterpret another app's namespace.

### Diagnostics

Map PDC diagnostics into `doctor` and document indexing results without preventing unrelated valid documents from being indexed. At minimum cover transport, envelope, version, duplicate document/block IDs, size/complexity, missing or mismatched assets, unsafe content, and unresolved links.

## Stages

### Stage 0 — contract fixtures

- Vendor or fetch an explicitly pinned corpus revision 3 for tests.
- Add fixture-root tests for both profiles, legacy HTML classification, hidden paths, symlinks, duplicate IDs, unsafe HTML, and no source mutation.

Exit: connector expectations are executable before production decoding changes.

### Stage 1 — Reader classification

- Add extensions and transport detection.
- Surface all PDC classification failures through existing skipped/doctor reporting.
- Do not change indexing yet.

Exit: every fixture is classified identically to the shared corpus.

### Stage 2 — decode and projection

- Add profile-specific text extraction and standard metadata.
- Add the canonical UUID mapping while retaining path-derived internal identity.
- Bump the decoder/projection version and rebuild document caches.

Exit: repeated rebuilds are deterministic and PDC UUID links resolve across moves.

### Stage 3 — cross-app verification

- Index the same copied vault produced by Oximemo and Sawhorse.
- Compare discovered IDs, titles, tags, links, assets, deleted state, and diagnostics with their Reader outputs.
- Verify raw `doc://` source is byte-identical and no source mtime changes.

Exit: all participating implementations pass corpus revision 3 and the same representative vault suite.

## Non-goals

- No PDC authoring, export, source repair, migration, asset garbage collection, or vault management.
- No new MCP tool solely for PDC.
- No conversion of documents into episodes.
- No use of PDC UUID as memory-plane entity, assertion, or occurrence identity.
- No network fetch while indexing HTML, CSS, links, images, or other subresources.

## Verification

```bash
cargo fmt --all -- --check
cargo test -p oxibrain-connectors -p oxibrain-store -p oxibrain
cargo clippy --all-targets --all-features -- -D warnings
cargo build -p oxibrain --no-default-features --features http-llm
```

The migration-chain up-test and document reprojection determinism test are mandatory when projection schema or decoder version changes.
