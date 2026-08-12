# Phase M4 Handoff — Token Auth, Client Crate, HTTP Transport, Eval → daemon / M5

> **Status:** Token-auth socket transport, `oxibrain-client` crate, loopback HTTP
> transport, and `eval` CLI subcommand all shipped. All gates green.
> **Branch:** `main` (squash-merge flow)
> **Predecessor:** M4 MCP-scope handoff (`docs/superpowers/handoffs/2026-08-12-m4-mcp-scope.md`)
> **Tests:** 196 pass, 0 fail. Clippy clean (`-D warnings`). Fmt clean. Standalone verified.

---

## 1. What this session shipped

Four of the five remaining M4 items from the prior handoff are done. The daemon
topology is now real: a client connects over a Unix socket, authenticates with a
token, and is gated by scope enforcement — or connects unauthenticated over
loopback HTTP.

| Capability (prior handoff §5) | Status |
|---|---|
| **Token auth on connections** | ✅ `serve_socket_auth` + `auth` handshake → scoped `BrainServer` |
| **`oxibrain-client` crate** | ✅ `BrainClient` — 7 tool methods, token auth, trusted + scoped modes |
| **HTTP transport** | ✅ `serve_http` — loopback-only POST→JSON-RPC (DESIGN §11.6) |
| **`eval` CLI subcommand** | ✅ `oxibrain eval --suite fast` — golden corpus, §14.2 gates |
| Daemon lifecycle (PID, restart) | ⬜ external supervision (launchd); socket is the foundation |
| Sampling LlmPort (§12.3) | ⬜ requires bidirectional JSON-RPC (server sends requests) |

---

## 2. Architecture: what was built

### Token auth on socket connections (`oxibrain-mcp`)

**`serve_socket_auth(brain, path)`** — the authenticated daemon transport. Each
connection must send a JSON-RPC `auth` request as its first line:

```jsonc
{"jsonrpc":"2.0","id":1,"method":"auth","params":{"token":"<secret>"}}
```

The server calls `brain.verify_token(&token)` → `Option<Scope>`. On success, a
`BrainServer::from_arc_scoped(brain, scope)` serves the rest of the session with
the scope enforced. On failure, the connection gets an `UNAUTHORIZED` error and
closes. The scope gate (`enforce_scope`) now has a real scope from a real token,
not just a test fixture.

**Refactoring:** `run_session` was split — the framing loop was extracted into
`session_loop(server, BufReader, BufWriter)` so `auth_session` can consume the
first line for the handshake, then delegate to the same loop. `BrainServer` gained
`from_arc` / `from_arc_scoped` constructors that take `Arc<Brain>`, so the daemon
shares one brain across many connections, each with its own scope.

### `oxibrain-client` crate (new)

Thin async client speaking newline-JSON-RPC over Unix sockets — the mirror of
`run_session`. Makes `Brain` one trait in embedded + daemon modes (P6):

```rust
// Embedded
let brain = Brain::open(BrainConfig::at("~/.oxi/brain")).await?;

// Daemon (trusted Unix socket)
let mut client = BrainClient::connect("/run/oxibrain.sock").await?;

// Daemon (authenticated)
let mut client = BrainClient::connect_with_token("/run/oxibrain.sock", &secret).await?;
```

**7 tool methods** mirror the MCP surface: `search`, `recall`, `get_entity`,
`ingest`, `declare`, `why`, `contradictions`. Plus `ping` for keepalive.

Protocol-level errors (scope denial, missing args) and tool errors (`isError`)
both map to `Err` with the server's message. Structured results parse to
`serde_json::Value` via `call_tool_json`.

**Zero oxibrain dependencies in the main lib** — the client only depends on
`serde_json`, `tokio`, `anyhow`. The test module pulls in `oxibrain` +
`oxibrain-mcp` for round-trip tests.

### HTTP transport (`oxibrain-mcp`)

**`serve_http(brain, addr)`** — loopback-only (DESIGN §11.6). Each HTTP POST body
is a JSON-RPC message; the response body is the JSON-RPC response. One request
per connection, no streaming — the simplest HTTP mapping of our newline-delimited
protocol.

Non-loopback binds are refused with a clear error: use a TLS-terminating reverse
proxy for remote access. No external HTTP framework — minimal manual HTTP parsing
over `tokio::net::TcpListener`.

### CLI growth

| Subcommand | What |
|---|---|
| `oxibrain serve --http <addr>` | Loopback HTTP transport (e.g. `127.0.0.1:8080`). |
| `oxibrain serve --socket <path> --require-token` | Authenticated daemon transport. |
| `oxibrain serve --socket <path>` | Unauthenticated socket — warns about filesystem-permission reliance. |
| `oxibrain eval --suite fast` | Golden corpus extraction eval — no network, deterministic. |

`eval` runs the same 3-fixture golden corpus as the M3 test suite. Prints
§14.2 metrics (fabricated-entity rate, precision, recall) and exits non-zero on
gate failure. The `fast` suite uses `FakeLlmPort` with fixture-replayed responses;
`full` requires a live provider (nightly CI).

A `debug_triples(space)` method was added to `Brain` and `oxibrain-store::query`
to extract (predicate, subject_surface, object_surface) triples through the store
boundary — avoids opening SQLite directly from the CLI.

---

## 3. Verification (this session)

- **Token-auth round-trip over real Unix socket**: issue token → connect → auth
  → tool call (contradictions) → success. Invalid token → UNAUTHORIZED → close.
  (2 MCP tests)
- **Client round-trip through real binary**: trusted socket → ping + ingest +
  contradictions. Authenticated socket → ingest + contradictions (Read+Ingest cap).
  Read-only cap denies ingest. Invalid token fails connection. (5 integration tests)
- **HTTP transport**: POST JSON-RPC ping to `127.0.0.1:18099` → 200 OK + result.
  Non-loopback bind refused. (2 MCP tests)
- **CLI `eval --suite fast`**: 5/5 triples extracted, precision 1.000, recall
  1.000, fabricated-entity rate 0.000. All §14.2 gates passed.
- **CLI `serve --help`**: `--http`, `--socket`, `--require-token` all visible.
- **Full suite**: 196 pass, 0 fail (was 190).
- **clippy** `--all-targets --all-features -D warnings`: clean.
- **fmt**: clean.
- **Standalone**: `oxibrain`, `oxibrain-mcp`, `oxibrain-client` cargo trees
  contain no `oxios-`/`oxicode-` crates; `--no-default-features --features http-llm`
  builds.

---

## 4. Test inventory

| Crate | Tests | Delta |
|---|---|---|
| oxibrain-ports | 4 | — |
| oxibrain-core | 70 | — |
| oxibrain-store (lib) | 22 | — |
| oxibrain-store (integration) | 10 | — |
| oxibrain (facade) | 8 | — |
| oxibrain-cli | 1 | — |
| oxibrain-llm-http | 3 | — |
| oxibrain-mcp | 26 | +4 (token-auth ×2, HTTP ×2) |
| oxibrain-client | 7 | **new** (2 unit + 5 integration) |
| **Total** | **196** | **+6 since last handoff** |

---

## 5. Remaining M4 work

1. **Sampling LlmPort (§12.3).** The server currently handles only incoming
   requests. Sampling requires the server to *send* a `sampling/createMessage`
   request to the client and await the response — bidirectional JSON-RPC.
   `run_session` / `session_loop` need to become a `select!` loop that also
   drains an outbound channel. The `LlmPort` impl delegates each `complete()`
   call to the client's model. Session-bound, `Sample` capability (off by
   default), `realtime`-only (§12.3). This is the largest remaining piece.

2. **Daemon lifecycle.** The socket is the foundation; `oxibrain-client` speaks
   it. Remaining: PID file management, restart policy (launchd supervises — the
   DESIGN explicitly delegates backgrounding to external supervision, §15: "the
   same artifact ... is the one launchd supervises"). Advisory-lock handling for
   the single-writer P8 contract when the daemon owns the store.

3. **Nested subcommand flattening.** CLI naming is flat kebab-case
   (`token-issue`, `predicate-list`). DESIGN §12.4 uses nested form
   (`token issue`, `predicate list`). A clap-flattening refactor across all
   commands. Cosmetic.

---

## 6. Key decisions

- **D-new-4 — Auth handshake is a JSON-RPC method, not a custom header.** The
  first line of a socket connection is `{"method":"auth","params":{"token":"..."}}`.
  This keeps one protocol (JSON-RPC 2.0) across all transports. The server
  responds with `{"result":{"authenticated":true}}` on success. No HTTP-only
  headers or binary framing — the same `run_session` loop serves post-auth.

- **D-new-5 — HTTP is POST→response, no SSE/streaming.** The simplest correct
  mapping of our newline-delimited JSON-RPC to HTTP. One request per connection.
  Long-running tasks (streaming ingest) are a §12.2 concern that lands with the
  sampling/bidirectional work. Loopback-only by design (§11.6); non-loopback
  requires TLS, which is a reverse proxy's job, not the in-house server's.

- **D-new-6 — `debug_triples` through the store boundary.** The eval command
  needs to extract triples from the projection. Rather than opening SQLite
  directly (which would violate the "only `oxibrain-store` may reference
  `rusqlite`" rule), a `debug_triples(conn, space)` function was added to the
  store's query module, exposed through `Brain::debug_triples`. Keeps the
  boundary intact.

---

## 7. Workspace changes

New crate: `crates/oxibrain-client/` — added to workspace members and deps.

New store function: `query::debug_triples(conn, space) -> Vec<(String, String, String)>`.

New Brain method: `debug_triples(space) -> Vec<ExtractedTriple>`.

New MCP exports: `serve_socket_auth`, `serve_http`, `BrainServer::from_arc` /
`from_arc_scoped`.

New CLI subcommands: `serve --http`, `serve --require-token`, `eval`.

---

End of handoff. The daemon topology ships: token-authenticated socket transport,
a client crate, loopback HTTP, and the eval suite. Start the sampling LlmPort
(bidirectional JSON-RPC) or M5 from here.
