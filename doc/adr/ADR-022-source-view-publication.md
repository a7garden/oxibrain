# ADR-022: Source-view publication across audiences

Status: implementation in progress; the contract below is selected. No production-store migration or operational acceptance is implied.

## Context

Ghost's approved bot plan requires selected private evidence to be published to
another audience, without copying private locators or counting recitations as
independent corroboration. This is separate from attaching existing spaces to a
bot through scoped read sessions.

Current `Scope` grants the Cartesian product of `spaces` and `caps`.
`EpisodeKind` distinguishes Primary, Declaration, and terminal Derived records.
`fold::compute_support` counts distinct episode identities. Consequently,
re-ingesting a published excerpt as a new Primary would give it an independent
source identity and could inflate support. An ordinary Derived summary avoids
extraction but does not itself provide publication authorization, correction
closure, or audience-safe citations. Neither is a complete publication API.

## Proposed contract

Publication is an explicit ledger operation, not a document writer or a new
Ghost-specific engine concept. It binds a stable caller occurrence, origin
reference and revision, exact selected view bytes and digest, destination space,
authorizing principal/policy revision, and explicit event time. The server
materializes the authorized source view and checks those bytes; a caller's
origin ID or trust claim is never sufficient evidence.

The engine checks origin read authority and destination contribution authority
separately. It must not combine read-private and write-shared into a token that
also grants write-private and read-shared. A scoped per-space grant extension or
an authenticated operation with two independently checked grants is necessary;
the final wire shape must be selected with the implementation. Legacy Scope
semantics remain unchanged. No additional MCP tool is required: an additive,
versioned mode of an existing write operation is preferred.

The ledger stores origin lineage in an engine-internal record. Audience-facing
reads return only an accessible publication citation, its view revision and
range. They must not expose origin space, private locator, source title, raw
origin IDs, relation metadata, or authorization secrets through reads, errors,
resource enumeration, or diagnostic output. Internal lineage never grants raw
cross-space access.

Repeated publication and re-citation retain the same root-evidence family.
Identical bytes from independent origin occurrences retain separate families.
Publication alone creates no new independent assertion or higher trust. Any
future extraction of published views must bind support to the original family,
preserve uncertainty and policy exclusions, and prove that the fold never
counts the same root twice. Until that path is implemented and tested,
publication views must remain non-extractable evidence.

Source correction, source retraction, destination publication withdrawal, and
loss of caller source access are distinct events. Correction/retraction closes
the current publication view through deterministic projection. Destination
withdrawal closes audience access. Caller access revocation blocks new
operations and in-flight delivery; it does not silently rewrite an already
authorized publication into a different event. These rules apply on cold replay
as well as incremental operation. Physical redaction requires a tested closure
over published copies; a public tombstone must not leak the private origin.

Ledger identity is the scoped source occurrence, never content equality. A
retry with identical parameters resolves the original receipt; changed bytes
under that occurrence conflict. Host I/O and engine commit are not one
transaction. Unknown acknowledgements require receipt reconciliation, not a
fresh occurrence. All derived relation/index state must be rebuilt from ledger
events. Source bytes on the document plane remain reader-owned and unchanged.

## Implementation and acceptance gates

Before promotion to an implemented architecture contract:

1. Select pure core types and authorization decisions, then store/facade
   sequencing under the existing P9 boundary. Freeze the versioned operation
   and declaration/replay format before allocating a schema migration.
2. Implement prior-schema migration fixtures, identical incremental/full truth
   reprojection, bounded lineage traversal, idempotent receipt recovery, and
   cold backup/restore coverage.
3. Test two private origins with equal bytes, repeated publication of one
   origin, chained re-citation, wrong revision/digest, unauthorized grants,
   grant revocation before delivery, and absence of new assertions on publish.
4. Test correction, withdrawal, retraction, and physical redaction closure in
   current and historical reads, including audience-safe errors/enumeration.
5. Verify Ghost's real source-view publication and correction flow against the
   engine contract. Separate fixture results from operational acceptance.

The present B2 separate-session adapter does not satisfy these gates. Until
they pass, Ghost must describe permanent private-to-shared publication as
unsupported rather than emulate it with text ingestion.


## Selected version 1 boundary

Version 1 bounds each selected view to 64 KiB, each materialized source and the
aggregate disposable publication projection to 8 MiB, and lineage to 4,096
views and 64 parent links. Oversized ordinary or compacted episode sources are
rejected before fetching source bytes. Predicate/entity-filtered write grants
cannot withdraw a complete view or retract a complete source family.

The durable operation is `source-view-publication/1`. It appends a typed
`PublicationV1` Declaration to the existing episode ledger, in the origin space.
It does not create another ledger plane, Primary episode, extracted assertion,
or document. A re-citation Declaration names only its accessible parent
publication citation and revision. Root lineage is computed in an internal
projection, never copied into a re-citation payload or destination response.
Destination withdrawal is a Declaration in the destination space containing only
the accessible publication identifier. Generic `declare` must reject these
reserved variants; only the checked publication facade admits them.

The request binds caller occurrence, origin kind/identity/revision, an exact
UTF-8 byte range, selected bytes and their BLAKE3 digest, destination, explicit
policy revision and event time. Publication identifier derives from destination,
authenticated principal and occurrence; the same identity with changed canonical
parameters conflicts. Byte digests do not identify sources. Root-evidence family
derives from source occurrence; publication chains preserve it. Version 1 never
extracts published views or counts them as independent support.

The facade accepts origin-read and destination-write credentials through a
non-serialized authentication argument. It verifies each independently against
current token state inside the short writer transaction, checks exact space and
capability, and checks them again before returning a receipt. Credentials never
enter Declaration JSON, model arguments, diagnostics or receipts. Legacy Scope
Cartesian semantics remain unchanged. Adding a wire mode must reuse an existing
operation and preserve the sixteen-tool cap.

Schema 19 adds only disposable publication projection tables. Every row and
closure is explained by a Declaration; full reprojection drops and rebuilds
them. The projection separates internal source/parent/family records from an
audience DTO with destination citation, view revision, view-relative range,
selected bytes and conservative uncertainty. Every audience read checks current
destination Read authority before lookup and immediately before delivery.
Errors use one opaque unavailable response; no private source identity, title,
locator, space, relation metadata or authorization data is returned.

Episode and document origins are distinct. Episode origins bind an existing live
Primary episode and exact content revision. Document origins bind a currently
configured, freshly materialized reader-owned document revision and decoder
revision; source files are never changed or promoted into episodes. An
unavailable or stale document cache cannot authorize a publication. Lifecycle
closure comes from a verified live source observation, not a caller's assertion
that a source changed. Its observed revision/event is bound into a Declaration
before current views are closed. Documents and brain stores are not one atomic
transaction: recheck the source revision before/after materialization and before
publication, withhold results on a race, and reconcile the original occurrence.
Audience delivery also pins the registered root specification, canonical root,
cache fingerprint/generation and exact raw revision. After the final current
credential check, a synchronous source check verifies those pins before forming
the audience DTO; a file edit or root replacement during the read withholds it.
An episode-only implementation is an incremental slice, not B6/A18 acceptance.

Correction/retraction closes affected current views and their bounded transitive
re-citations. Destination withdrawal also closes descendants which rely on that
view. Access revocation blocks new publication/read delivery but does not alter
an authorized historical publication event. Physical redaction expands the
closure to every Declaration containing copied bytes, erases those bytes and
rebuilds safe tombstones; neither current nor historical audience reads may
recover redacted bytes. Cycles, excess depth and oversized closures fail closed.

The implementation must prove schema-18 upgrade, incremental/full projection
identity, reopen/restore receipt identity, independent equal-byte origins,
re-citation family conservation, generic destination read isolation, stale
revision rejection, verified document correction, withdrawal and redaction
closure, and in-flight revocation. These remain gates until tests are recorded.
