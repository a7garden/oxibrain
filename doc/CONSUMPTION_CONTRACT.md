# Consumption Contract 1.1

> `ARCHITECTURE.md` §19.2. This document pins the public surface consumers depend on and
> the stability guarantees each tier carries. It is the contract between
> oxibrain and its ecosystem consumers (oxios-kernel, oxiline, oximemo, Claude
> Desktop, third-party MCP clients).
>
> **1.1 (2026-08-17)** — adds the planned additive client surface for the Oxi Foundation v1
> contract: `BrainEndpoint`, `default_socket_path`, `connect_default`,
> `connect_endpoint`, `ClientHello`, and `ServerInfo`. None of these are shipped in
> `oxibrain-client@0.2.0`; they are pinned to land in `oxibrain-client@0.3.x`. Existing
> auth-first-message and `Scope`/`Capability` semantics are preserved unchanged.

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
| **Stable** | `pub` in `oxibrain::*` | Semver-protected. Signature changes are breaking. | `Brain`, `BrainConfig`, `Brain::open`, `Brain::ingest`, `Brain::query`, `Brain::assemble_context`, `Brain::declare`, `Brain::beliefs`, `Brain::redact`, `Brain::export_jsonl`, `Brain::import_jsonl`, `Episode`, `SourceRef`, `TrustTier`, `EpisodeKind`, `BrainError`, `Timestamp`, `Scope`, `Capability`, `TokenInfo`, `Declaration`, `EntityRef`, `DeclObject` |
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
- `Brain::ingest_note(space, path, content, occurred_at) -> Result<String>`
- `Brain::ingest(space, content, source, trust, extractor_id) -> Result<String>`
- `Brain::get_episode(id) -> Result<Option<Episode>>`
- `Brain::episode_count() -> Result<i64>`

### Query

- `Brain::query(q) -> Result<RankingResult>`
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
- `Brain::extract_pending(space, config, budget) -> Result<ExtractSummary>`
- `Brain::reextract(space, config) -> Result<ExtractSummary>`
- `Brain::consolidate(space, config) -> Result<Vec<String>>`
- `Brain::summarize_communities(space, config) -> Result<usize>`
- `Brain::job_status() -> Result<Vec<(String, usize)>>`

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
`Declaration`, `EntityRef`, `DeclObject`.

## Compatibility test

A compile-time test in `crates/oxibrain/src/compat.rs` verifies the stable
surface. If any method is removed or its signature changes incompatibly, the
compatibility test fails to compile. Consumers can pin the same test against
their version to detect breaking changes.

## Planned additive client surface (Oxi Foundation v1, oxibrain-client 0.3.x)

The Foundation v1 contract (`doc/spec/oxi-foundation-v1.md`, ADR-007) introduces
discovery and capability handshake as **additive** `oxibrain-client` features. None of
these change the existing auth-first-message rule, the `Scope`/`Capability` model, or
the fifteen-tool MCP surface; they make discovery and version negotiation possible
without bolting on a sixteenth tool. They are **planned**, not yet shipped in
`oxibrain-client@0.2.0`. Hosts pinned to 0.2.0 keep working unchanged.

### Discovery helpers

- `pub fn default_socket_path() -> PathBuf` — returns `~/.oxi/brain/oxibrain.sock`,
  honoring `$OXIBRAIN_SOCKET` when set. Pure function, no I/O.
- `pub fn connect_default() -> impl Future<Output = Result<BrainClient>>` — convenience
  over `connect_endpoint(BrainEndpoint::default())`.
- `pub fn connect_endpoint(endpoint: BrainEndpoint) -> impl Future<Output = Result<BrainClient>>`
  — opens the connection, performs the `ClientHello`/`ServerInfo` handshake, and only
  then returns a ready-to-use client. If the daemon's `schema_version` is unknown to the
  client, the function returns a typed error; the client has **not** silently downgraded.

### Endpoint and handshake types

```rust
pub struct BrainEndpoint {
    pub socket: PathBuf,
    pub token: Option<Arc<str>>,   // resolved by the host; never persisted here
    pub hello: ClientHello,
}

impl Default for BrainEndpoint {
    fn default() -> Self { /* default_socket_path() + default ClientHello */ }
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
Within `oxibrain-client@0.3.x` the additions are non-breaking; anything that would break
an existing 0.2.0 caller goes into a future major. `default_socket_path` is a pure
function with no failure modes and may be relied upon by hosts pinned to 0.3.x for the
rest of the v1 lifecycle.

### Auth-first-message and scope semantics, preserved

The existing rule — a token (or anonymous-on-Unix-socket flag) is presented before any
payload — is unchanged. The `Scope`/`Capability` model from `ARCHITECTURE.md` §15.1–§15.2
remains the only authority on what a connection may do. `ClientHello` and `ServerInfo`
are **metadata only**: they never carry a token, never widen a scope, and never replace
a `Scope` check. A host that prefers to bypass the handshake (for example, a CI runner
that already knows the daemon is at the default path) may continue to call the existing
constructor; the additive surface is opt-in by the host.
