# Changelog

All notable changes to oxibrain are documented here. Conventional commits;
squash-merged.


## [0.11.0] — 2026-08-29

Storage footprint (ADR-014, ARCHITECTURE.md v2.14): the ranking half no
longer multiplies content ~7×. Measured on a real instance: 7.7 MB →
6.6 MB total, FTS content tables gone, orphan source rows deleted.

### Architecture (schema v12/v13, documents v2)

- **Brain FTS becomes contentless** — `fts_word`/`fts_ngram` keep only
  their inverted indexes (`content=''`, `contentless_delete=1`) with a
  small `fts_map` rowid table; `episodes.content` is the single text
  source. The v13 migration repopulates both indexes from the ledger and
  deletes **orphan source rows** (sources that never produced an episode —
  the tempdir-path leak; referenced rows stay as provenance).
- **Compacted episodes stay searchable** — `rebuild_fts` reads the
  effective text (`content`, or `content_compacted` when compacted);
  pre-v13 a compacted episode silently vanished from search.
- **Entity vectors go int8 (v12)** — `entity_vectors` becomes a plain BLOB
  table with scale-1.0 symmetric quantization (encoder output is
  L2-normalized); KNN is an exact Rust-side integer L2 scan. Migration
  converts float rows in place. 4 KB → 1 KB per entity.
- **TF-IDF vectors go int8** — quantized against each vector's max-abs
  component (cosine is scale-invariant): 4 KB → 1 KB per row; old f32 rows
  are replaced by the next rebuild.
- **documents.db v2** — `doc_texts` is the single decoded-text copy with
  external-content FTS (search SQL unchanged) and plain int8 `doc_vectors`
  (re-embed via `index --embed` after upgrade).
- **Quarantine is a retry queue** — a successful extraction consumes its
  matching `extraction_failures` rows.
- **Doctor** reports orphan source counts.

## [0.10.1] — 2026-08-28

### Fixed

- **Plan tokens are stateless** — the in-process plan table made every CLI
  dry-run → commit pair refuse with `plan_stale` (each `oxibrain <op>` is a
  new process, so the commit never saw the dry-run's plan; CLI `redact`,
  which mandates a token, was uncommittable). Tokens now self-verify:
  `hex(ts ‖ blake3(op ‖ closure_hash ‖ ts))`, re-derived against the fresh
  closure at commit. TOCTOU safety, TTL, and the agent contract unchanged;
  single-use dropped (replay protection stays with `idempotency_key`).
  (ADR-013 amendment.)
- **`plan_stale` is a first-class CLI outcome** — it was misclassified as
  `internal` (exit 9, `retryable: false`); now `error.code = "plan_stale"`,
  exit 6 (conflict slot, exits stay contiguous 1–9), `retryable: true`.
- `oxibrain-client` inherits the workspace version again (the one hand-pinned
  member; the 0.10.0 bump caught it).

## [0.10.0] — 2026-08-28

The agent-first CLI contract (ARCHITECTURE.md v2.13, ADR-012/013, spec
`doc/spec/agent-first-cli-v1.md`): the CLI and MCP become two transports
over one op registry, built for agent callers — predictable envelopes,
explicit space, dry-run rails, counted budgets.

### Architecture (ARCHITECTURE.md v2.13)

- **One op registry** — new crate `oxibrain-ops` is the single source of
  truth for the tool surface: MCP `tools/list`, the CLI `oxibrain <op>`
  dispatch, `oxibrain schema`, and the generated agent skill all derive
  from it. MCP tools 15 → **14** (cap holds; `stats`/`review_merges`
  became admin CLI subcommands, `resolve` moved in).
- **Agent contract (ADR-013)** — every op takes a JSON payload with
  `space` REQUIRED (no implicit creation; unknown space errors with the
  `space add` hint); responses use one envelope `{api, ok, op, space?,
  data, meta}`; exit codes are machine-meaningful (`locked` is a
  first-class outcome, exit 5, with `wait_lock_ms`); stderr is for
  humans, stdout is for the caller. Ids are never guessed — surfaces
  resolve via `resolve`/`search` only.

### Breaking

- CLI human verbs (`remember`, `ask`, `spaces`, `review`, …) are replaced
  by the 14-op payload dispatch (`oxibrain remember` now reads the same
  JSON payload as the MCP tool; legacy spelling removed, not deprecated).
- `declare`/`retract`/`merge_entities`/`redact` validate predicate names
  against the registry at runtime — unknown predicates are
  `invalid_input`, not silently `internal`.

### Safety rails (P6)

- **Capability-filtered `tools/list`** — a scoped MCP session sees only
  the ops its `Scope.caps` allow; read-only callers never learn a
  mutating op exists.
- **Plan tokens (dry-run → commit)** — `dry_run: true` returns
  `{plan: {token, closure_hash, expires_at, affected}}` and writes
  nothing; committing presents the token, and the server re-derives the
  closure from current ledger state — a moved ledger refuses with
  `plan_stale`. `redact` REQUIRES a plan token on every commit.

### Hardening (P4) and instrumentation (P5)

- Validators for 64-hex ids, locators, timestamps, `idempotency_key`;
  untrusted retrieved text is wrapped with provenance
  (`untrusted_content`), never inlined raw.
- Read ops report `meta.tokens` (model-tokenized, budget-bound
  projections) and `meta.dropped` (what filters/truncation discarded) —
  an empty result is not "nothing exists" until `dropped` agrees.

### Agent skill (P7)

- **`admin skill install`** — generates `SKILL.md` + `CONTEXT.md` from
  the op registry (targets: `omp`, `claude`, `raw`), so the skill cannot
  drift from the live surface.

### Toolchain

- **Rust 1.96.1, aligned with the oxi ecosystem** — `rust-toolchain.toml`
  pins 1.96.1 (oxios/oxiline/oxicode run the 1.96 line); workspace
  `rust-version` and `clippy.toml` MSRV move to 1.96; CI installs
  `dtolnay/rust-toolchain@1.96`. Comments that cited the old 1.85 floor
  (MCP hand-rolled JSON-RPC rationale, `human_format` pin, consumer-smoke
  note) updated to match.

## [0.9.0] — 2026-08-28

### Features

- **Space lifecycle management** — explicit named spaces with
  `oxibrain space add|remove`; `documents.toml` roots carry a mandatory
  `space` field; per-space vaults with a default space in config.


## [0.8.0] — 2026-08-27

The daemon is gone. The vault becomes a rebuildable document cache; the
memory half is the only durable state. oxibrain-core is no longer reachable
over a long-lived socket — every process is caller-owned (stdio child or
foreground HTTP). The MCP tool cap holds at 15; one native RPC is added.

### Architecture (ARCHITECTURE.md v2.11)

- **Two planes, two stores** — `brain.db` carries the immutable episode
  ledger and the projection; `documents.db` is a disposable cache that can
  be rebuilt from the configured `documents.toml` roots at any time.
  `Brain::search()` returns `SearchResponse { memory, documents, freshness }`
  — the two lists never blend scores. `SearchPlane::{Memory, Documents}` and
  `Query.planes` make the split explicit. Legacy `document` /
  `document_revision` ledger rows are excluded from default memory search
  and `recall`; `legacy_document_history` is the only surface for them.
- **Handle-free facade, op-scoped lock** — `Brain` no longer carries a
  long-lived `StoreHandle`. Every read method opens the store in
  `SQLITE_OPEN_READONLY` for that op; every write takes a short
  per-process lock via a `lockfile` advisory (bounded retry
  `[25, 50, 100, 200, 400, 800] ms`). Multi-writer safety is the
  cooperation of the facade + the lock, not a daemon. (P8 retained as a
  cooperation rule; the requirement still applies.)
- **No sync, no watcher, no queue.** `ingest_jobs` is dropped in
  `schema v11`; the missing rows live as the query
  `uncached_memory_episodes` (deduped, content-hash-keyed). Backlogs
  cannot silently rot. The carry-over 1024 pull-source rows that the old
  watcher tried to mount are kept as retired `sources` so the FK chain
  (196 episodes) is preserved, but they have no live effect; doctor
  shows them only in the `legacy pull sources (provenance-only)` section.

### Removed

- `oxibrain daemon` subcommand, the `serve --daemon` path, the default
  `~/.oxi/brain/oxibrain.sock` socket, `~/.oxi/brain/.oxibrain.pid`,
  `extract_pending` background loop, and `oxibrain::daemon` module.
  `BrainClient::connect_default` / `connect_endpoint` /
  `endpoint_default_path` / `default_socket_path` are deleted.
- `crates/oxibrain-store/src/watch.rs` (vault watcher).

### Features

- **`serve --stdio` and `serve --http` (caller-owned)** — `--stdio` keeps
  the process alive until the consumer closes stdin; `--http` binds a
  foreground server. No sockets to discover, no `bootout` to forget.
- **`BrainClient::spawn_local` / `spawn_local_with_token`** — wraps a
  stdio child in `LocalProcessEndpoint { executable, dir }` so a child
  started by the client is reaped with `kill_on_drop`.
- **`documents.toml` (canonical config) + `index --documents` CLI** — the
  root table is `[[root]]` with `alias, path, space, include?, exclude?,
  max_file_bytes?`. `oxibrain init` seeds a single `vault` root when the
  resolved data directory is the default AND the user did not pass `--dir`
  AND `~/.oxi/vault` exists (spec §4 canonical config). `index --documents
  [--embed]` reconciles + applies chunks + optionally embeds.
- **Native RPC `document_history`** — returns the per-locator revision
  chain from the documents cache (`DocumentRevision { locator, revision,
  modified_at, content_hash, content }`). Read-capability gated like
  `resources/read`. Not a 16th MCP tool.
- **`extract --pending`** — replaces the old `extract_pending` queue
  drain. Pulls rows from `uncached_memory_episodes`, dedupes by
  `(space, content_hash)`, and exits when done. No daemon, no PID,
  no launcher.
- **`BrainClient::document_history` + `DocumentRevision` DTO** — read
  surface for the cache.
- **`oxibrain-client@0.8.0`** — fork of the workspace version (lockstep
  was always a mistake: client is the deployment boundary, not the
  engine). The 0.7.0 client remains compatible with 0.8.0 server.
- **Per-note revision history — `Brain::episodes_for_locator` (Consumption
  Contract 1.3)** — the read side of the vault occurrence chain (§4.2.1):
  every episode ingested for `<dir>/<locator>`, oldest first, full content
  per revision. Native RPC `episodes/for_locator` (read capability gated;
  not a 16th MCP tool). Client `oxibrain-client@0.7.0` adds
  `BrainClient::episodes_for_locator` + the `EpisodeSummary` DTO. ADR-011
  records why vault git history stays consumer-owned
  (`oxi-vault-git@0.1.0`) and why the read-only occurrence query is the
  read-side complement.
## [0.6.0] — 2026-08-20
### Features

- **Daemon-hosted vault watch (ADR-010)** — the C4 loop closes: the daemon
  adopts every registered pull source into a debounced watcher (2 s quiet;
  unchanged re-scans are no-ops via content-hash classification), so vault
  edits become episodes without any consumer-side code. Registration lives in
  the store and survives restarts.
- **`sync/run` native RPC + daemon attach** — `oxibrain sync <dir>` now works
  with a running daemon: on the P8 advisory lock it attaches over the default
  socket and runs the pass via the new RPC (register → sync → adopt watcher).
  Scoped sessions need `trusted_ingest` + target-space membership.
- **`oxibrain::vault` module** — `sync_vault` / `pull_sources` moved out of
  the CLI so CLI, RPC, and watcher share one implementation (P6).
  `BrainClient::sync_run` returns the typed `SyncOutcome` DTO. `Brain` is
  now `Clone` (cheap Arc'd handle) for watcher threads.
- Docs: ARCHITECTURE v2.9, CONSUMPTION_CONTRACT 1.2, ADR-010; the 2026-08-20
  space-enumeration design's "consumer-owned watch" note is superseded.

## [0.5.0] — 2026-08-20

### Features

- **Space enumeration, end to end** — spaces are discoverable, not just
  creatable: `ledger::list_spaces` (canonical `(created_at, id)` order with
  live episode/entity counts), `Brain::list_spaces` + `SpaceInfo` on the
  stable facade surface (compat-registered, consumption-contracted), the
  native `spaces/list` JSON-RPC method and `spaces://` static resource on the
  daemon (both scope-filtered), a typed `BrainClient::list_spaces()` returning
  the client-owned `SpaceSummary` DTO, and the read-only `oxibrain spaces`
  CLI verb (`Brain::open_ro` — no advisory lock, coexists with a running
  daemon). The fifteen-tool MCP cap is untouched: first-party operations ride
  the native RPC layer beside `handshake` and `reproject`.
- **ADR-009: topology unification deferred, honestly** — §16.1 no longer
  promises a one-trait `Brain::connect`. `Brain` is the embedded surface,
  `oxibrain-client::BrainClient` the remote surface (ECOSYSTEM C6);
  unification is post-v1 with a stated trigger. CONSUMPTION_CONTRACT 1.1
  gains `list_spaces`, `lookup_space`, and `SpaceInfo`.

### Fixes

- **`resources/read` scope bypass (security)** — resource reads skipped the
  capability + space-membership gate that tool calls enforce; a scoped token
  could read `space://` URIs of spaces outside its scope. All non-`spaces://`
  schemes are now gated before any database work; `spaces://` self-filters to
  the session's membership. Regression-tested.
- **Scope gates no longer write on denial** — both `enforce_scope` and the new
  resource gate resolved space ids via write-creating `ensure_space`, leaving
  shadow space rows behind denied probes (an existence-enumeration side
  channel). They now use the read-only `Brain::lookup_space`. Resource reads
  also check `scope.expires_at`, matching tool calls.

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
