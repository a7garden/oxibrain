# Source-view lifecycle CLI

`oxibrain admin source view` exposes the same versioned facade operations as the
native authenticated source-view RPC. It adds no MCP tool. The engine publishes
an explicitly selected source view; generated prose and arbitrary claims are
not publication inputs.

Commands read one JSON object from stdin, at most 1 MiB. Duplicate object keys,
trailing values and unsupported fields/versions are refused. Credentials are
supplied only through named environment handles. Handle names use uppercase
ASCII letters, digits after the first character, or underscores, up to 128
characters. A missing/empty handle or one containing a malformed credential
fails closed. Tokens are not command arguments, serialized payload fields,
receipt fields or error output.

```sh
oxibrain --dir STORE admin source view publish \
  --origin-token-env ORIGIN_READ_TOKEN \
  --destination-token-env DESTINATION_WRITE_TOKEN < publish.json

oxibrain --dir STORE admin source view read \
  --destination-token-env DESTINATION_READ_TOKEN < read.json

oxibrain --dir STORE admin source view withdraw \
  --destination-token-env DESTINATION_WRITE_TOKEN < withdraw.json

oxibrain --dir STORE admin source view retract \
  --origin-token-env ORIGIN_WRITE_TOKEN < retract.json
```

Publish needs independent origin Read and destination Write authority; the
engine never unions their scopes. Read requires destination Read. Withdraw
requires destination Write, and source retraction requires origin Write.
Credentials are authenticated against current token state by the facade.

Every payload has `version: "source-view-publication/1"`. Publish has exactly
`version` and `request`, where `request` is `PublishViewV1`: `occurrence_id`,
`origin`, `range`, `bytes`, `digest`, `destination`, `policy_revision`, and
`occurred_at`. Ranges count UTF-8 bytes and must select exactly the supplied
bytes from the current authenticated source revision. Digests use the existing
publication BLAKE3 format. `occurred_at` is UTC Unix milliseconds and cannot
claim a future occurrence. Document origins carry their exact canonical UUID where present,
root/internal locator identity, current source revision and decoder. A source
correction requires a fresh source observation, not a caller-claimed replacement.

Read has `destination` and `publication_id`. Withdraw adds `occurrence_id` to
those fields. Retract has `origin` and `occurrence_id`. Credentials, trust claims,
and caller-generated root families are not accepted in any payload.

Stdout is one JSON envelope with `api: 1`, `ok`, `op`, `status`, `summary`,
`data` on success, `next_actions`, and `artifacts`. Errors have a stable code,
safe retry instruction and stop condition; arbitrary engine errors and input
bytes are withheld. Exit codes are 0 success, 2 invalid input, 3 unavailable
reference, 4 authority unavailable, 5 locked/busy, 6 conflict, 7 budget, or 9
unconfirmed internal failure.

Publish returns the original minimal receipt on an unchanged same-occurrence
retry, including after a lost acknowledgement. It does not resurrect withdrawn
bytes. Destination reads expose only the selected view, accessible citation,
view-relative range, conservative uncertainty and lifecycle state. They never
expose private root identities, raw source IDs, unselected suffixes or an
independent-support claim. Closed views return no bytes. Withdraw/retract retain
the immutable ledger and raw source; no source files are rewritten or deleted.

The CLI tests launch the actual binary against an isolated store, exercise all
four operations and a cold reopen, verify exact credentials and receipt replay,
and check that errors/receipts do not echo secrets or unselected source bytes.
This documents source behavior; it does not imply an installed binary upgrade,
production publication or completed operational A18 acceptance.
