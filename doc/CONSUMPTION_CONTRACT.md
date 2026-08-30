# Consumption Contract 1.5

> `ARCHITECTURE.md` §19.2. This document pins the public surface consumers depend on and
> the stability guarantees each tier carries. It is the contract between
> oxibrain and its ecosystem consumers (oxios-kernel, oxiline, oximemo, Claude,
> third-party MCP clients).
>
> **Version note (2026-08-27, v2.11 cutover):** the coordinated breaking-version bumps
> ship together — oxibrain workspace crates `0.6.0 → 0.7.0` and
> `oxibrain-client 0.7.0 → 0.8.0`. The changes are listed below; every removal is
> paired with a replacement at the same release, and the MCP tool surface stays at
> fifteen.
>
> **1.1 (2026-08-17)** — adds the planned additive client surface for the Oxi Foundation v1
> contract: `BrainEndpoint`, `default_socket_path`, `connect_default`,
> `connect_endpoint`, `ClientHello`, and `ServerInfo`. **All of these were superseded
> by 1.4 (2026-08-27)** and never shipped: the v2.11 daemonless cutover removed the
> listening socket before any of them landed.
> **1.2 (2026-08-20)** — daemon-hosted vault watch (ADR-010): native RPC
> `sync/run`, client `BrainClient::sync_run` + `SyncOutcome`, the
> `oxibrain::vault` module (`sync_vault`, `pull_sources`, `SyncReport`,
> `PullSource`), and `Brain: Clone` (cheap handle — Arc'd store actor and
> caches). **All of 1.2 was superseded by 1.4** and removed in v2.11: no daemon,
> no watcher, no `sync/run` RPC, no `BrainClient::sync_run`, no `SyncOutcome`,
> no `oxibrain::vault`. `Brain: Clone` survives — it now means a handle-free
> runtime facade (§3.1, P8), not a shared store actor.
> **1.3 (2026-08-23)** — per-note revision history: `Brain::episodes_for_locator`
> (stable Query), `oxibrain::vault::episodes_for_vault_file` (dir-based read-only
> resolution), native RPC `episodes/for_locator`, and client
> `BrainClient::episodes_for_locator` + `EpisodeSummary` (client 0.7.0). **All
> of 1.3 was superseded by 1.4** and removed in v2.11: documents no longer
> become memory episodes, so there is no occurrence chain to query. The
> replacement is gix-backed `Brain::document_history` (ADR-011 amendment).
>
> **1.4 (2026-08-27) — daemonless two-plane cutover.** The resident daemon is
> gone. `Brain` becomes a handle-free runtime facade. Documents move to a
> separate `documents.db` cache rebuilt from configured files and gix history.
> Native RPC `document_history` joins `handshake`, `reproject`, `spaces/list`,
> `document_history` (read-gated like `resources/read`); native RPCs `sync/run`
> and `episodes/for_locator` removed; `default_socket_path` /
> `connect_default` / `connect_endpoint` / `BrainEndpoint` removed.
>
> **1.5 (2026-08-27) — space lifecycle.** Workspace crates (including
> `oxibrain-client`) move `0.8.0 → 0.9.0` together; the changes are §1.5 below.
> The MCP tool surface stays at fifteen.
>
> **1.6 (2026-08-30) — unified Oxi home.** No shared config and no global
> default space; the canonical layout is `spaces/<space>/vault` plus
> app-private subtrees. New additive client surface:
> `BrainClient::register_document_root` + `RegisterDocumentRootRequest` /
> `RegisterDocumentRootOutcome` over the native `register_document_root`
> method; apps never edit `documents.toml`.

## 1.5 (2026-08-27) — Space lifecycle

1. **Breaking (minor):** verbs and MCP tools no longer implicitly create
   spaces. Create with `oxibrain space add` / `init` / archive import.
   Unknown spaces fail fast with a `space add` hint.
2. **Additive:** MCP tools omitting `space` resolve the default from
   `~/.oxi/config.toml` (`default_space`), not a hardcoded `"personal"`.
3. **Additive:** CLI `space add|remove|default`; `RedactTarget` serde gains
   `{"kind":"space","id":…}`. No new MCP tools; fifteen-tool cap unchanged.
4. **Breaking (unstable tier):** the `oxibrain-mcp` server entry points
   `serve_stdio` / `serve_stdio_at` / `serve_http` / `run_session_gated`
   each gain a `default_space: String` parameter — an arity change on every
   signature (`BrainServer::with_default_space` is the builder alternative).
   `oxibrain-mcp` is feature-gated unstable (see *Stability tiers*), so this
   rides the minor bump like the rest of 1.5.
## 1.6 (2026-08-30) — Unified Oxi home

1. **Breaking (policy, enforced):** `documents.toml` is oxibrain-owned.
   Consumers register document roots through `Brain::register_document_root`
   or the native JSON-RPC method `register_document_root`; direct edits are
   a contract violation. The facade op is idempotent (added / replaced /
   unchanged, upsert keyed by alias) and omits-unknown-rules turn into
   connector defaults.
2. **Additive:** `BrainClient::register_document_root(request) -> outcome`
   with `RegisterDocumentRootRequest { space, alias, path, include?,
   exclude?, max_file_bytes? }` and `RegisterDocumentRootOutcome
   { outcome, root }`.
3. **Compatibility:** legacy homes (`~/.oxi/models`, flat `~/.oxi/vault`,
   `~/.oxios`, `~/.oxicode`) are read-only fallbacks for one release;
   migrations are journaled + resumable (`oxibrain admin migrate
   [--dry-run]`) and never delete sources. `OXI_HOME` overrides the root
   on every app.

## Versioning

- **Semver** on the `oxibrain` crate facade.
- Within a major version (0.x): additive changes only to the public API.
  Breaking changes require a minor-version bump during 0.x (semver pre-1.0
  convention) and a major-version bump at/after 1.0.
- MCP tool schemas: additive only within a major. New tools, new optional
  parameters, and new resources are non-breaking. Removed tools, changed
  parameter types, or changed required parameters are breaking.

## Stability tiers

| Tier | Marker | Guarantee | Examples |
|---|---|---|---|
| **Stable** | `pub` in `oxibrain::*` | Semver-protected. Signature changes are breaking. | `Brain`, `BrainConfig`, `Brain::open`, `Brain::ingest`, `Brain::search`, `Brain::index_documents`, `Brain::document_history`, `Brain::remember`, `Brain::assemble_context`, `Brain::declare`, `Brain::beliefs`, `Brain::redact`, `Brain::export_jsonl`, `Brain::import_jsonl`, `Episode`, `SourceRef`, `TrustTier`, `EpisodeKind`, `BrainError`, `Timestamp`, `Scope`, `Capability`, `TokenInfo`, `Declaration`, `EntityRef`, `DeclObject` |
| **Unstable** | feature-gated | May change between minor versions. Opt-in via Cargo feature. | `oxibrain-llm-http` (LLM adapter), `oxibrain-mcp` (MCP server internals) |
| **Internal** | `pub` in non-facade crates | No guarantee. `pub` for workspace reasons only. | Everything in `oxibrain-store`, `oxibrain-core`, `oxibrain-index`, `oxibrain-connectors` |

## The stable surface

The `oxibrain` crate re-exports everything consumers need. The public API is:

### Engine

- `Brain::open(config) -> Result<Brain>`
- `Brain::with_clock(config, clock) -> Result<Brain>`
- `Brain::with_llm(config, clock, llm) -> Result<Brain>`

### Ingestion

- `Brain::ensure_space(name) -> Result<String>`
- `Brain::lookup_space(name) -> Result<Option<String>, BrainError>`
- `Brain::list_spaces() -> Result<Vec<SpaceInfo>>`
- `Brain::ingest_note(space, path, content, occurred_at) -> Result<String>`
- `Brain::ingest(space, content, source, trust, extractor_id) -> Result<String>`
- `Brain::get_episode(id) -> Result<Option<Episode>>`
- `Brain::episode_count() -> Result<i64>`

### Search

- `Brain::search(query) -> Result<SearchResponse>` — `{ memory, documents,
  freshness }` over both planes; `query.planes(...)` selects `Memory`,
  `Documents`, or both (default both). Two-plane search is opt-in; a single-plane
  caller sees the same `memory` shape as prior versions.
- `Brain::assemble_context(space, query, budget) -> Result<ContextResult>`
- `Brain::beliefs(space, entity_id) -> Result<Vec<Belief>>`
- `Brain::beliefs_as_of(space, entity_id, valid_at) -> Result<Vec<Belief>>`
- `Brain::contradictions(space) -> Result<Vec<Statement>>`
- `Brain::traverse(space, spec) -> Result<TraversalResult>`
- `Brain::timeline(space, entity_id, from, to) -> Result<Vec<TimelineEntry>>`
- `Brain::diff(space, entity_id, at_a, at_b) -> Result<DiffResult>`
- `Brain::why(space, statement_id) -> Result<ExplainBlock>`
- `Brain::resolve_entity_id(space, ty, surface) -> Result<Option<String>>`
- `Brain::list_entities(space, limit) -> Result<Vec<Entity>>`
- `Brain::list_merges(space) -> Result<Vec<EntityMerge>>`

### Document plane

- `Brain::index_documents(IndexOptions) -> Result<DocumentFreshness>` — diff
  `documents.toml` against the `documents.db` manifest, scan each root, apply
  the `ApplyPlan`; `--embed` walks pending vector chunks.
- `Brain::document_history(space, alias, locator, limit) -> Result<Vec<DocumentRevision>>`
  — read-only gix-backed history (`{ revision, committed_at_ms, content }`).
  Replacement for the retired `Brain::episodes_for_locator` occurrence-chain
  read path; revisions are anchored in consumer-owned git, not in the ledger.

### Mutation

- `Brain::declare(space, decl) -> Result<String>`
- `Brain::redact(target, reason, actor) -> Result<RedactionResult>`
- `Brain::redact_dry_run(target) -> Result<RedactionClosure>`

### Lifecycle

- `Brain::reproject() -> Result<()>`
- `Brain::rebuild_indexes(space) -> Result<()>`
- `Brain::rebuild_communities(space) -> Result<()>`
- `Brain::apply_decay(space) -> Result<usize>`
- `Brain::compact(space) -> Result<usize>`

### Extraction

- `Brain::extract_one(space, episode_id, config) -> Result<ExtractSummary>`
- `Brain::extract_one_with(space, episode_id, config, llm) -> Result<ExtractSummary>`
- `Brain::extract_uncached(limit) -> Result<usize>` — bounded inline pass over
  the un-extracted backlog under the write lock; queue-less — extraction is
  inline on the append path.
- `Brain::pending_extraction_stats() -> Result<ExtractionStats>` —
  `{ count, oldest_seq }` without walking the whole ledger.
- `Brain::remember(space, content, source) -> Result<CaptureOutcome>` —
  `Captured { episode_id }` or `CapturedPending { episode_id, pending_count }`
  when the model call fails; the next `extract_uncached` pass picks the episode up.
- `Brain::reextract(space, config) -> Result<ExtractSummary>`
- `Brain::consolidate(space, config) -> Result<Vec<String>>`
- `Brain::summarize_communities(space, config) -> Result<usize>`

### Security

- `Brain::issue_token(scope, issued_by, label) -> Result<(TokenInfo, String)>`
- `Brain::verify_token(secret) -> Result<Option<Scope>>`
- `Brain::revoke_token(id) -> Result<()>`
- `Brain::list_tokens() -> Result<Vec<TokenInfo>>`
- `Brain::audit_log(limit) -> Result<Vec<AuditRow>>`

### Export/Import

- `Brain::export_jsonl() -> Result<String>`
- `Brain::import_jsonl(jsonl) -> Result<()>`

### Types

All re-exported from `oxibrain::*`:
`Brain`, `BrainConfig`, `Episode`, `EpisodeKind`, `SourceRef`, `TrustTier`,
`BrainError`, `ClockPort`, `LlmPort`, `LlmRequest`, `LlmResponse`, `SystemClock`,
`Timestamp`, `Capability`, `CapabilitySet`, `Scope`, `TokenInfo`, `AuditEntry`,
`RedactTarget`, `RedactionClosure`, `RedactionResult`, `AuditRow`,
`Declaration`, `EntityRef`, `DeclObject`, `SpaceInfo`.

## Compatibility test

A compile-time test in `crates/oxibrain/src/compat.rs` verifies the stable
surface. If any method is removed or its signature changes incompatibly, the
compatibility test fails to compile. Consumers can pin the same test against
their version to detect breaking changes.

## Client transport surface (1.4, shipped)

`oxibrain-client@0.8.0` (paired with the `oxibrain` facade at `0.7.0`) replaces
socket discovery with a **caller-owned stdio child**. There is no daemon, no
listening socket, and no default-path lookup: every session starts by spawning
`oxibrain serve --stdio --dir <dir>` as a child of the caller and speaking
JSON-RPC over its stdin/stdout. The child dies with stdin; the caller owns the
lifecycle. `serve --http <addr>` is the foreground loopback variant for the
operations console (`ARCHITECTURE.md` §16.6).

### Spawn helpers

- `pub fn spawn_local(endpoint: LocalProcessEndpoint) -> Result<BrainClient>` —
  builds the `Command`, spawns the child, performs the `handshake` JSON-RPC, and
  returns a ready-to-use client. If the child exits before handshake, returns a
  typed error; the client has **not** silently downgraded.
- `pub fn spawn_local_with_token(endpoint: LocalProcessEndpoint, token: TokenInfo) -> Result<BrainClient>`
  — spawns the child with a token presented as the first message after
  handshake, preserving the auth-first-message rule on the wire.

### Endpoint and handshake types

```rust
pub struct LocalProcessEndpoint {
    pub executable: PathBuf,    // path to the `oxibrain` binary
    pub dir: PathBuf,           // the brain data directory (`--dir`)
}

pub struct ClientHello {
    pub client_version: &'static str,   // "oxibrain-client/<crate_version>"
    pub protocol_version: u32,           // 1 for the v1 contract
    pub supported_features: &'static [&'static str],
}

pub struct ServerInfo {
    pub server_version: String,
    pub schema_version: u32,
    pub supported_features: Vec<String>,
    pub requires_client_features: Vec<String>,   // mandatory for this connection
}
```

### Stability

These additions follow the same **additive-only** rule as the rest of this contract.
Within `oxibrain-client@0.8.x` the additions are non-breaking; anything that would break
an existing 0.7.x caller goes into a future major. `LocalProcessEndpoint` is a plain
data struct (`executable`, `dir`) with no I/O and may be relied upon by hosts pinned
to 0.8.x for the rest of the v1 lifecycle. The previous
`default_socket_path` / `connect_default` / `connect_endpoint` / `BrainEndpoint`
additive surface (1.1) never shipped — see the **Version note** at the top of this
document.

### Auth-first-message and scope semantics, preserved

On a scoped session the rule is unchanged: a token is presented as the first
message after handshake, before any payload. The `Scope`/`Capability` model from
`ARCHITECTURE.md` §15.1–§15.2 remains the only authority on what a connection may
do. `ClientHello` and `ServerInfo` are **metadata only**: they never carry a token,
never widen a scope, and never replace a `Scope` check. A host that prefers to
bypass the in-process handshake (for example, a test that already wired the child)
may call the existing constructor with a pre-opened transport; the handshake is
opt-in by the host.

## Document-plane surface (1.4, shipped)

The two planes — memory (`brain.db`) and documents (`documents.db`) — share one
`Brain` facade and one MCP/JSON-RPC surface. There is no separate document daemon;
documents are a disposable cache rebuilt from `documents.toml`-configured files and
gix read history (§4.2.1 of `ARCHITECTURE.md`, ADR-011). Document roots come only
from `<dir>/documents.toml` (`[[root]]` rows carry `alias`, `path`, `space`,
`include`/`exclude`, `max_file_bytes`); the brain never opens or writes repos
directly — gix access lives in `oxi-vault-git`, owned by oximemo and oxios.

Document-plane methods on the facade:

- `Brain::index_documents(IndexOptions { embed: bool, budget: Option<Budget> }) -> Result<DocumentFreshness>`
  — load `documents.toml`, diff against `documents.db`'s manifest, scan each root
  (plain or git-backed), apply the `ApplyPlan` in one transaction; `--embed` walks
  pending vector chunks through the configured `EmbeddingPort`.
- `Brain::document_history(space, alias, locator, limit) -> Result<Vec<DocumentRevision>>`
  — read-only gix-backed history (rev id, committed-at ms, content). Powers
  `oxibrain document-history`, the MCP `doc://<alias>/<locator>?rev=<rev>` resource,
  and the native JSON-RPC `document_history` method.
- `Brain::search(query) -> Result<SearchResponse>` — `{ memory, documents, freshness }`
  over both planes; `query.planes(planes!())` selects which planes participate
  (`Memory`, `Documents`, or both; default both). Document hits carry freshness
  metadata; memory hits are unchanged from prior versions.
- `Brain::pending_extraction_stats() -> Result<{ count: u64, oldest_seq: Option<u64> }>`
  — read-only queue length and oldest un-extracted sequence.
- `Brain::extract_uncached(limit: usize) -> Result<usize>` — bounded inline pass
  over the un-extracted backlog; queue-less — `remember` / explicit `ingest` /
  `capture` append the episode inline and return immediately.

Document-plane methods on `oxibrain-client::BrainClient`:

- `BrainClient::search(query) -> Result<SearchResponse>` — same shape as the facade.
- `BrainClient::document_history(space, alias, locator, limit) -> Result<Vec<DocumentRevision>>`
  — native JSON-RPC `document_history`; read-gated like `resources/read`.
- `BrainClient::pending_extraction_stats() -> Result<ExtractionStats>`
- `BrainClient::extract_uncached(limit) -> Result<usize>`
- `BrainClient::handshake(hello: ClientHello) -> Result<ServerInfo>` — first call on
  any session; capability negotiation, never a token replacement.
- `BrainClient::ping() -> Result<ServerInfo>` — liveness + schema-version probe.

`SearchResponse { memory, documents, freshness }` and `DocumentRevision { revision,
committed_at_ms, content }` are client-owned DTOs that mirror the native JSON-RPC
wires (`ARCHITECTURE.md` §16.2, §19.2). Legacy `document` / `document_revision`
memory episodes are preserved on disk but excluded from memory search, context
assembly, and extraction — doctor reports them as a legacy section and migration
preserves them across schema upgrades.
