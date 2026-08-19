# Changelog

All notable changes to oxibrain are documented here. Conventional commits;
squash-merged.

## [Unreleased]

## [0.4.0] — 2026-08-19

### Features

- **Event identity, trust policy, and server-evaluated trust** — episodes gain
  an identity tuple `(space_id, source_id, occurrence_id)` distinct from
  `content_hash`, which is now integrity-only. Schema v10 rebuilds the episodes
  table (drops `UNIQUE(space_id, content_hash)`) and adds `sources`,
  `source_policies`, and `assertions.trust`. The ledger gains `insert_event`
  with `IngestAttachment` and occurrence-based dedup (same content re-push is
  idempotent; different content creates a new episode). `RegisterSource` /
  `SetSourcePolicy` write policy state through the ledger (P1: ledger stays the
  only durable write path). The fold computes support per distinct episode
  across trust tiers, and assertions carry their episode's trust into belief
  confidence. MCP enforces the trust gate in `enforce_scope` — `trust=trusted`
  requires the new `trusted_ingest` capability; `ingest`/`remember` use the
  event path with server-built attachments. Facade: `Brain::ingest_event` /
  `ensure_source`.
- **Pull connector occurrence identity** — `sync` registers the vault as a
  pull source, derives occurrence chains via
  `H(source_id, locator, predecessor, content_hash)`, and ingests through the
  event path. Legacy episodes participate in `Unchanged` classification but are
  never re-ingested.
- **Curation parity (P4 exit condition)** — entity merge/split/alias/retract,
  `declare`, predicate add, and source policy on the CLI. New
  `Split`/`Alias`/`RegisterPredicate` declaration variants project
  deterministically: `Split` undoes the latest active merge, `Alias` adds a
  `UserDeclared` entity key, `RegisterPredicate` writes the predicates table.
  Every correction emits an auditable `Declaration`; reprojection remains
  byte-identical.
- **Embedded repair/operations console (ADR-008)** — `apps/brain-ui` scoped to
  seven routes (Overview, Entity, Conflicts, Merges, Failures, Sources,
  Operations); `ask`/`capture`/`graph` surfaces and the sigma/graphology deps
  removed. `dist/` is committed so `cargo install oxibrain-cli && oxibrain serve
  --http` renders the console with no Node toolchain; `--ui-dir` remains a dev
  override. CI gates: clean bun build, committed `dist/` must match, gzipped
  bundle ≤ 400 KB.
- **`reproject` over JSON-RPC** — a bare method (deliberately not an MCP tool —
  too destructive for agent access; fifteen-tool cap preserved) returning
  before/after space stats: `{completed_at, entities_reprojected,
  statements_updated, before, after}`. Completes the Operations view's
  reproject button.
- **`review_merges` sections** — the MCP tool gains a `section` parameter
  (`merges|failures|sources`) so the console's FailuresView/SourcesView reuse
  an existing tool instead of adding new ones. Adds `Brain::list_failures` /
  `Brain::list_sources`; `SourceRow` now serializes.
- **HTML note scanning** — `oxibrain-connectors` vault scan ingests `.html`
  notes alongside `.md` (oximemo format): `split_frontmatter` parses the
  leading `<!-- +++ … +++ -->` comment, `html_to_text` strips tags/entities/
  comments and drops `script`/`style` contents so FTS sees clean prose. Scan
  rules mirror oximemo: case-insensitive `.md`/`.html`, skip
  `TEMPLATE.md`/`.html`, `oximemo.toml` (+ legacy sibling), `_assets/`, and
  hidden directories.

### Documentation

- ARCHITECTURE.md v2.6 → v2.9 (memory authority redesign, curation parity,
  pull connector occurrence identity, Plan D minimal console §16.6), ADR-008
  (console technology) accepted.
- ECOSYSTEM.md v2 verb-ownership blueprint; implementation plans for curation
  parity and pull connector occurrence identity.

## [0.3.0] — 2026-08-17

### Features

- **Oxi Foundation v1 contract (frozen schema)** — `doc/spec/oxi-foundation-v1.md` and ADR-007
  define three-plane topology (Oxi Foundation = provider/profile/package shape only, never a
  runtime crate), the canonical Foundation v1 wire format (`profiles.json` with
  `credential{service,account}` and dotted role names; `packages.lock` with `sha256-<hex>`
  digests and dotted abstract requirements), and the canonical listening socket
  `~/.oxi/brain/oxibrain.sock` (override via `$OXIBRAIN_SOCKET`). ECOSYSTEM.md v1.0 publishes the
  three-plane picture; ARCHITECTURE.md v2.5 and CONSUMPTION_CONTRACT.md v1.1 keep Brain authority
  unchanged and document the additive-only client surface.
- **Foundation profile parsing & local-first LLM resolution** — `oxibrain-cli` reads Foundation
  v1 profiles at the CLI boundary only. Role-aware resolution ladder: explicit
  `OXIBRAIN_LLM_PROVIDER` → Foundation profile → compat env (`OXIBRAIN_ANTHROPIC_API_KEY` /
  `OPENAI_API_KEY`) → local GGUF default. Secret resolution is a sealed `SecretResolver` trait;
  the OS-keychain resolver lives behind `feature = "os-keychain"` (no leakage into
  core/store/index). Provider mechanism (JsonSchema vs ToolCall) is derived from the provider
  before role binding.
- **Stable daemon discovery + compatibility handshake** — `oxibrain-client 0.3.0` adds a typed
  `ClientHello` (advertised `min_compatible` / `max_compatible` / `client_version` /
  `min_store_format_version` / `supported_operations`) and `ServerInfo` protocol over
  `~/.oxi/brain/oxibrain.sock`. Auth-first-message preserved; the server rejects out-of-range
  requests with `HandshakeError::IncompatibleProtocol`. New helpers: `connect_default`,
  `connect_endpoint`, `BrainEndpoint`. `serve --daemon` defaults to the canonical socket path,
  refuses live owner, and removes stale sockets only under advisory lock + live-PID match.
- **Typed packages.lock reader** — `oxibrain_client::foundation_package` reads `packages.lock`
  with `select_package_for_target` (pure), validates digest format (`sha256-<64 lowercase
  hex>`), gates each requirement against the abstract allow-list, and preserves
  `AbstractRequirement::Unknown(s)` verbatim (never silently dropped). Scope byte-identical with
  legacy manifests; hostile lockfiles cannot induce scope drift.
- **Deterministic consolidation under Foundation profiles** — `consolidate_impl` and
  `summarize_communities_impl` now thread a `provider_profile_id` into `ExtractorConfig::id()`
  so cache provenance is the only thing that changes when the profile changes (truth-fold bytes
  remain unchanged on `None`). Single-sqlite-tx writer-actor discipline with dual-channel
  `(tx, etx)` and `(rx, erx)` channels for real error propagation. Community-summary sources
  are restricted to `kind='primary'` (`hash_community_member_set('community', ...)` is the
  namespace boundary).

### Fixes

- **Stale-socket probing**: `set_permissions(0o700)` propagates on a freshly-created parent
  directory; broad pre-existing parents get `warn!` instead of silent propagation. Server no
  longer refuses a legitimate serve when a third party has loosened parent dir permissions to
  `0o755`.

### Documentation

- ARCHITECTURE.md v2.5, CONSUMPTION_CONTRACT.md v1.1, ECOSYSTEM.md v1.0, ADR-007 accepted.
- Cross-host fixture corpus `tests/fixtures/oxi-foundation/v1/` is byte-identical with the
  oxicode mirror (10 fixtures) and the parser outcome table is enforced by
  `cross_host_fixture_corpus_{profiles,packages}_match_outcome_table`.
- E2E smoke `e2e_smoke_default_discovery` (#[ignore]) — daemon launches under the default
  socket, typed handshake returns `BrainCapabilities { BrainProtocolVersion(1), "oxibrain"
  v0.3.0 }, ingest → search return typed results, post-stop degradation observed in ~22 µs.

## [0.2.0] — 2026-08-16

### Features

- **Local GGUF extraction wired into the CLI** — extraction works with no API
  key: `OXIBRAIN_LLM_PROVIDER=local` (the default when no key is set) opens the
  GGUF from the model manifest, grammar-constrained (§7.4).
- **Lazy model pull on first extraction use** (ADR-005) — `oxibrain init` stays
  instant and offline; the extract model downloads automatically on the first
  `extract`/`reextract`, resumable, digest-verified. `OXIBRAIN_MODELS_DIR`
  points at a pre-pulled directory for air-gapped installs. `init` prints a
  one-line hint.
- **`oxibrain sync`** — idempotent vault sync (mtime-anchored) from a directory
  of markdown notes.
- **Registry: multi-type entity objects** — `ObjectKind::Entity` type set;
  relaxed subject types for containment/alias predicates.
- **`ANTHROPIC_BASE_URL` override** for the HTTP provider.

### Fixes

- reextract surfaces and records per-episode failures (invalid LLM output goes
  to `extraction_failures`, never silently dropped); CLI extraction max_tokens
  2048 → 8192.
- `oxibrain-llm-local`: decode-bounds and batch fixes for long prompts.
- `import-oxios` passes the resolved `space_id` to ingest.

### Documentation

- `doc/ARCHITECTURE.md` v2.3: §1.3/§8.4 rewritten around lazy pull.
- ADR-005 accepted and implemented.

## [0.1.0] — 2026-08-15

Initial release: episode ledger + knowledge projection, CLI, MCP server, local
LLM/embedding.
