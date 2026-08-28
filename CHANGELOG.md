
## [0.10.0] — 2026-08-28

Storage footprint: contentless FTS, int8 vectors, junk-path elimination
(ADR-014, plan `doc/plans/2026-08-28-storage-footprint.md`).

### Storage (brain.db)

- **v12 — entity embeddings as int8 in a plain BLOB table.**
  `entity_vectors` drops vec0 (sqlite-vec 0.1.x classifies every INSERT
  blob as float32) for a plain table with fixed-scale symmetric int8
  (`quantize_i8_fixed`, `clamp(v, -1, 1) * 127`) and Rust-side exact
  integer L2 KNN. 4 KB → 1 KB per entity. Migration converts existing
  float rows in place.

- **v13 — contentless FTS5 + `fts_map`.** The FTS layer previously stored
  the full body twice (fts_word_content + fts_ngram_content shadow
  tables) on top of `episodes.content`: ~3× the text bytes. Both indexes
  are now `content='', contentless_delete=1` with a small rowid map;
  `episodes.content` is the single text copy. SQLite ≥ 3.43 required
  (rusqlite 0.32 bundles 3.46).

- **Compacted episodes stay searchable.** Pre-v13, `compact_episodes`
  cleared `episodes.content` and the rebuild indexed empty text;
  compacted episodes silently left search. Rebuild paths now use the
  effective content (`effective_episode_content(content, content_compacted)`),
  mirroring `ledger::get_episode`'s transparent decompression.

- **Orphan sources deleted at migration.** A source row no episode
  references (e.g. a tempdir root from a test run) is removed:
  `DELETE FROM sources WHERE id NOT IN (SELECT source_id FROM episodes WHERE source_id IS NOT NULL)`.
  Rows still referenced stay (provenance, P2). `doctor` reports the
  count since.

- **`extraction_failures` cleared on success.** A successful
  `cache_response` now `DELETE FROM extraction_failures WHERE episode_id=?1
  AND extractor_id=?2`. The quarantine is a retry queue, not an archive.
  Redaction is still the only other deleter.

### Storage (documents.db)

- **v2 — `doc_texts` + external-content FTS + plain int8 vectors.**
  `doc_texts` is the single decoded-text copy; both `doc_fts_word` /
  `doc_fts_ngram` are external-content on it (search SQL unchanged).
  `doc_vectors` drops vec0 for a plain int8 BLOB table; embeddings are
  recomputed on the next `index --embed`. FTS deletes are rowid-mediated
  through `doc_texts` — external-content tables leave orphan postings on
  column-predicate DELETE.

### Index (oxibrain-index)

- **Symmetric int8 quantization.** New `quantize_i8` /
  `dequantize_i8` (cosine-safe, per-vector max-abs rescale) and
  `quantize_i8_fixed` (L2-safe, scale 1.0) helpers.
- **TFIDF stored as int8.** `rebuild_tfidf` writes
  `quantize_i8(vector)`; `load_knn_index` decodes with
  `dequantize_i8`. 4 KB → 1 KB per row at dim 1024; old f32 rows are
  replaced on the next rebuild.

### Measured on the reference store (303 episodes)

| | Before | After |
|---|---|---|
| `fts_word_content` | 368 KB | gone |
| `fts_ngram_content` | 368 KB | gone |
| Orphan sources | 978 rows | 0 |
| TFIDF row size | 4 KB | 1 KB |
| `brain.db` | 4.9 MB | 4.9 MB |
| Directory (with cleanup) | 13 MB | **7.6 MB** |

The `brain.db` size is dominated by the FTS inverted indexes
(`fts_ngram_data` 808 KB, irreducible; the §7.4 chunk-level-only
mitigation is still in the toolbox), `episodes` table, and the int8
vectors. Per-episode marginal cost ≈ 5× + 1 KB (down from ≈ 5× + 4 KB).

# Changelog

All notable changes to oxibrain are documented here. Conventional commits;
squash-merged.


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
