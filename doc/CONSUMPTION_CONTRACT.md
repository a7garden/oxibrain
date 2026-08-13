# Consumption Contract 1.0

> `ARCHITECTURE.md` §19.2. This document pins the public surface consumers depend on and
> the stability guarantees each tier carries. It is the contract between
> oxibrain and its ecosystem consumers (oxios-kernel, oxiline, oximemo, Claude
> Desktop, third-party MCP clients).

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
