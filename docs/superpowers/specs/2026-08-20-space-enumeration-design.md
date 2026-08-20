# Space Enumeration & First-Party RPC — Design

> **Date:** 2026-08-20 · **Status:** approved for implementation (user standing
> instruction: autonomous overnight progression, superpowers flow)
> **Scope:** close the space-discovery gap end-to-end; lock the `Brain` topology
> decision; fix a resources scope bypass found during investigation.

## 1. Problem

Three verified gaps (evidence from this session's investigation):

1. **No space enumeration anywhere.** `ensure_space` exists
   (`crates/oxibrain/src/lib.rs:118`, `crates/oxibrain-mcp/src/server.rs:112`),
   but no API lists spaces: no store fn, no facade method, no CLI verb, no MCP
   tool/resource, no client method. `space://{name}` requires knowing the name.
   Agents cannot discover "which spaces exist" — creation without discovery.
2. **`Brain::connect` (§16.1 "one trait, both modes") is unimplemented.** The
   facade has `open`/`open_ro` only; the remote path is the separate
   `BrainClient` type. Doc-implementation mismatch.
3. **First-party clients are bound to MCP tool semantics.** `BrainClient`
   mirrors the fifteen `tools/call` tools with raw JSON returns; the native
   non-tool RPC layer (`handshake`, `reproject`, `ping`) has no first-party
   data operations.

Plus one security defect discovered while reading the dispatch code:

4. **`resources/read` bypasses `enforce_scope`.** Only `tools/call` gates by
   capability + space membership (`server.rs:374-375`). An authenticated
   scoped-token session can read `space://other` resources it is not scoped
   to. Violates §15.1 "no query, traversal, or write crosses [a space
   boundary]" — resources are queries.

## 2. Goals

- `list_spaces` on every surface: store → facade → CLI → MCP resource →
  native RPC → client typed helper.
- Scope-correct enumeration: a scoped session sees only its spaces.
- Fix the `resources/read` scope bypass.
- Lock the topology decision in ADR-009 and align §16.1 with it.

## 3. Non-goals (with reasons)

- **`Brain::connect` implementation** — ADR-009 defers; see §5.
- **`CLI --watch`** — §16.4 documents it but vault watching is consumer-owned
  (ECOSYSTEM C3: oxibrain reads through connectors; oximemo's vault connector
  watches). A CLI polling watch would duplicate that contract. Recorded as a
  known doc gap; separate decision if wanted.
  *(Superseded 2026-08-20 by ADR-010: the watch is brain-owned and
  daemon-hosted — the P8 single-writer lock leaves no other automatable
  placement. `docs/../doc/adr/ADR-010-daemon-hosted-vault-watch.md`.)*
- **Consumer-side integrations** (oxicode `memory.*` mapping, oxios migration)
  — other repos, explicitly tracked there (ECOSYSTEM §3.5–§3.6).
- **Full typed client mirror of the Brain surface** — client-owned DTOs per
  operation (pattern established here by `SpaceSummary`); a complete mirror is
  a 1.0-scale stability decision.

## 4. Design

### 4.1 Store (`oxibrain-store/src/ledger.rs`)

`list_spaces(conn) -> Result<Vec<SpaceRow>, BrainError>` beside
`create_space`/`get_space`. Decision-free fetch (P9). `SpaceRow`:
`{ id, name, created_at: i64 (millis), episode_count: i64, entity_count: i64 }`,
ordered by `(created_at, id)` — canonical, deterministic.

SQL: one statement, subselect counts:

```sql
SELECT s.id, s.name, s.created_at,
  (SELECT COUNT(*) FROM episodes e WHERE e.space_id = s.id) AS episode_count,
  (SELECT COUNT(*) FROM entities en WHERE en.space_id = s.id) AS entity_count
FROM spaces s ORDER BY s.created_at, s.id
```

### 4.2 Facade (`oxibrain`)

`SpaceInfo` DTO in `models.rs`: `{ id: String, name: String,
created_at: Timestamp, episode_count: i64, entity_count: i64 }` —
`Timestamp`, never bare `i64` in the public signature (AGENTS.md).
`Brain::list_spaces(&self) -> Result<Vec<SpaceInfo>, BrainError>` — read-only
(`read_op!` path). Registered in `compat.rs` and `CONSUMPTION_CONTRACT.md`
(additive; stable surface grows, nothing breaks).

### 4.3 MCP (`oxibrain-mcp/src/server.rs`)

- **Native RPC `spaces/list`** routed beside `reproject` in `handle`:
  NOT a tool — the fifteen-tool cap is untouched. Result:
  `[{ id, name, created_at, episode_count, entity_count }]`.
- **Static resource `spaces://`** in `resources_list`; `resources_read`
  handles scheme `spaces` returning the same JSON. Consistent with the
  existing `space://{name}` template.
- **Scope rules (both paths):** `scope: None` → all spaces (trusted local
  channel); `Some(scope)` → filter to `scope.spaces` membership. Enumeration
  is itself a query over space existence; names are inside the privacy
  boundary.
- **Security fix:** `resources_read` gains scope enforcement before any DB
  work: resolve the target space name (path for `spaces://`/`space://`,
  `?space=` defaulting to `personal` for the others) and require membership
  when a scope is present. `spaces://` under scope returns the filtered list
  (not an error) — listing what you may see is the useful scoped behavior.

### 4.4 Client (`oxibrain-client`)

`call_rpc_json(method, params) -> Result<Value>` low-level helper (generalize
`ping`'s raw-RPC shape). `SpaceSummary` DTO owned by the client crate
(`{ id, name, created_at_ms: i64, episode_count: i64, entity_count: i64 }`) —
no dependency on unstable core types; millis on the wire, caller converts.
`BrainClient::list_spaces(&mut self) -> Result<Vec<SpaceSummary>>` calls
`spaces/list` and parses.

### 4.5 CLI (`oxibrain-cli`)

`oxibrain spaces` → table: `NAME ID CREATED EPISODES ENTITIES`. Uses
`Brain::open_ro` — pure read; no advisory lock; coexists with a running
daemon (lib.rs:100-102). This is the first CLI verb on the read-only path;
if it proves out, migrating other read verbs is a follow-up, not tonight.

## 5. Topology decision (ADR-009, summarized)

Measured facts: `Brain` holds `Arc<StoreHandle>` (writer actor),
`Arc<dyn ClockPort>`, `Option<Arc<dyn LlmPort>>`, `Arc<dyn TokenizerPort>`,
`Option<Arc<dyn EmbeddingPort>>`, `Arc<Mutex<ResolutionCache>>`.
`with_llm`/`extract_one_with` accept trait objects unspeakable over a socket.

- **(i) Enum inner** (`Embedded | Remote(BrainClient)`): every one of ~40
  methods gains a remote arm; LLM-injecting methods cannot be remote; the
  unified surface would lie about those methods.
- **(ii) Trait extraction**: `compat.rs` references `Brain::method` as
  inherent fn pointers; a trait split means rewriting the stable-surface
  test and the consumption contract for zero current consumers.
- **(iii) Chosen — defer; align the doc.** Transports already live in
  `oxibrain-mcp`/`oxibrain-client`; ECOSYSTEM C6 routes consumers through
  `oxibrain-client`. §16.1's one-trait line is revised to state the two
  typed surfaces and the unification trigger (first consumer needing runtime
  topology switching). Locked in `doc/adr/ADR-009-brain-topology-deferred.md`.

## 6. Testing

- Store: unit test — create spaces in controlled clock order, assert
  ordering, counts, and zero-count spaces appear.
- Facade: `list_spaces` round-trip after `ensure_space` + ingest.
- MCP: unscoped lists all; scoped filters to membership; scoped
  `resources/read space://other` → `UNAUTHORIZED` (regression test for the
  bypass); `spaces://` resource contract (exact keys); `spaces/list` RPC
  shape.
- Client: socket round-trip against a spawned server (pattern of the
  existing round-trip tests).
- CLI: run `cmd::spaces::run` against a tempdir store, assert output lines.
- Gates: `cargo test --workspace`, `cargo clippy --all-targets --all-features
  -- -D warnings`, `cargo fmt --all -- --check`, standalone guarantee build.

## 7. Approval note

User standing instruction (this session): proceed autonomously overnight,
follow superpowers. The brainstorming approval gate is therefore satisfied by
this documented self-review; design choices above carry their evidence
inline.
