# Post-M12 retrieval fixes

Date: 2026-10-24 (Asia/Seoul).

The owner authorized implementation of the first two bounded retrieval fixes
identified in [the post-M12 study](2026-10-23-post-m12-improvements.md): section
expansion for heading hits and space eligibility before document dense top-k.
The other research proposals remain follow-up work.

## Section candidates

Search retains its existing small chunks. Recall builds a complete matching
heading section from the same verified decoded view, ending at the next heading
of equal or lower level and retaining descendant sections. Short heading-free
documents can offer their complete decoded body. The pure selection and evidence
assembly live in core; connectors supply transient heading ranges; the facade
sequences cache lookup, source verification, and packing.

Markdown heading ranges come from pulldown-cmark 0.13.4 with the applicable GFM
options. This distinguishes actual headings from fenced code, comments, raw HTML
blocks, and indented code, and supports setext headings. The existing HTML and
legacy Djot scanners collect heading positions in their decoded views. HTML
positions are translated during whitespace collapse, after entity decoding.
Legacy HTML uses this structure only when it reproduces the existing decoded
view exactly.

Search and recall share materialization. Each selected document is read and
decoded once per candidate pass, under the existing materialization byte bound.
Expanded evidence bytes are also bounded, and duplicate section identities are
removed before packing. PDC UUIDs, revisions, decoder identity, decoded digests,
and UTF-8 source ranges remain attached. Source bytes and cached decoded text
are unchanged, so no document conversion, cache schema migration, or decoder
version bump is needed. A source revision mismatch cannot return an expansion
with old coordinates.

Packing still counts the final canonical context with the configured tokenizer.
A smaller heading excerpt, when available, reports shortening and omitted ranges
against the complete section. An expanded section cannot fall back to a command
chunk that omits a later condition; it offers a heading-only excerpt or is omitted
when the complete section does not fit. A budget-limited result does not claim
complete support merely because its heading fitted.

## Dense scope

`DocumentCache::knn` now requires a space. Its vector fetch joins live cached
chunks/documents and excludes other spaces and trashed documents before scoring,
sorting, and truncation. Missing vectors are absent from that channel. Equal
distances use chunk identity as the deterministic tie-breaker. The facade also
retains its defensive scope check during materialization.

The fix applies to both dense and hybrid document search. It does not add dense
session retrieval or change recall's current lexical document channel.

## Regression coverage

The public facade cases exercise canonical Markdown, canonical HTML, readable v1
Djot/HTML, plain Markdown, and legacy HTML. They assert answer plus condition,
child-section inclusion, peer-section exclusion, unchanged source bytes, UUID and
decoded-view citation integrity, complete long-section support, and strict token
budgets. A source-race case rejects a changed revision before materialization and
reads the new revision on a subsequent recall.

Dense regressions hold an eligible K=1 result fixed while adding closer vectors
from another space, in both dense and hybrid modes. Store cases cover trash,
missing vectors, empty spaces, zero limit, and stable distance ties. A property
test varies section contents while checking parent/child/peer boundaries. CLI and
MCP fixtures also separate a heading from its command and condition.

## Remaining work

Dated follow-up: the remaining implementation and real-model experiments are
recorded in [the follow-up report](2026-10-24-post-m12-followup.md). The statements
below describe the scope at completion of this first change.

The live reader gate, optional semantic source channels, channel-specific
coverage receipts, query-relevant complete truth-group selection, and repeated
tokenization measurements remain open. These fixes establish the covered
retrieval behaviors, not general answer quality or semantic-model performance.

## Validation

Final local verification used Rust 1.99.1:

- `cargo test --locked -p oxibrain-connectors -p oxibrain-core -p oxibrain-store -p oxibrain -p oxibrain-cli -p oxibrain-mcp`: 926 passed, zero failed, one existing slow measurement ignored; includes both PDC corpus runners, byte-identical truth reprojection, writing-system parity, CLI, MCP, and doctests.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo build --locked -p oxibrain --no-default-features --features http-llm`: passed.
- `cargo tree --locked -p oxibrain --prefix none`: no `oxios-` or `oxicode-` dependencies.
- `git diff --check`: passed.
- `cargo deny check`: failed on the existing Tokio 1.53.1 advisory `RUSTSEC-2026-0386`; licenses, sources, and bans passed. Both HEAD and the working lockfile pin Tokio 1.53.1. No advisory exemption was added.

The checkout already contained unrelated toolchain, CI, rendering, index, and
migration edits before this implementation. They remain uncommitted and are not
credited to these retrieval fixes. No live reader/model evaluation, release,
production-store migration, or user-document rewrite was performed.
