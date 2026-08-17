# Oxi Foundation and Shared Brain Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the standalone `oxibrain` daemon the sole durable-memory data plane for Oxi applications, while adding a versioned Oxi Foundation contract for daemon discovery, non-secret provider/model profiles, Keychain-backed credentials, and portable skill/persona packages.

**Architecture:** Oxi Foundation v1 is a versioned filesystem/protocol contract, not a required shared Rust crate or a broker. `oxibrain` remains standalone and local-first. It exposes an additive discovery and handshake surface through `oxibrain-client`; optional profile-selected LLM adapters implement `LlmPort` at the facade/CLI boundary. Oxicode and Oxios use the client and their own capability enforcement. Brain consolidation remains a derived-episode operation, never note editing or mutable app memory.

**Tech Stack:** Rust 2024, Tokio, JSON-RPC over Unix socket, `oxibrain-client`, `oxibrain-ports::LlmPort`, OS Keychain platform adapters, serde/JSON Schema, existing SQLite ledger/projection.

## Global Constraints

- Preserve `doc/ARCHITECTURE.md` P1–P11, especially P1 ledger/projection, P5 no deletion through consolidation, P7 ports at the boundary, P8 one writer, and P9 decision/data separation.
- `oxibrain-core`, `oxibrain-store`, and `oxibrain-index` MUST NOT depend on Oxicode, Oxios, an Oxi Foundation runtime crate, provider SDKs, or the OS Keychain.
- A default build remains local-first and pulls zero `oxios-*` or `oxicode-*` crates. Local GGUF inference remains the C2 default and works without keys or network access.
- Oxi Foundation v1 is shared configuration plus versioned schemas. It MUST NOT introduce a mandatory model gateway, daemon broker, or a second durable memory store.
- Store no raw credential in `~/.oxi`; profiles contain only a Keychain service/account reference. Environment variables remain an explicit development/automation override.
- `BrainClient` is an additive stable surface under `doc/CONSUMPTION_CONTRACT.md`. Do not rename or weaken existing methods or authentication/scope behavior.
- Keep the MCP tool count at fifteen. Discovery and compatibility use transport/RPC handshake metadata, not a sixteenth MCP tool.
- A Foundation package declares abstract requirements only. It cannot grant capabilities, bypass a host approval flow, or cause every package to be injected into every prompt.

---

## Cross-Repository Contract to Freeze First

The three implementation tracks share this exact v1 contract. Implement it before host-specific behavior so that each host produces and consumes identical data.

```text
~/.oxi/
  foundation/v1/profiles.json      # non-secret, schema-versioned provider profiles
  foundation/v1/packages.lock      # resolved package name/version/digest/source/trust
  brain/oxibrain.sock              # default local daemon socket
  brain/                            # store and daemon state owned only by oxibrain
```

`profiles.json` contains `schema_version`, profile `id`, provider kind, endpoint, model id, declared model capabilities, allowed roles, and a `{ service, account }` Keychain locator. It never contains an API key, OAuth access token, or refresh token. Role bindings are `memory.extract`, `memory.consolidate`, `coding.primary`, and `assistant.general`; an application selects a role and resolves a permitted profile, not an arbitrary provider secret.

`packages.lock` resolves immutable packages. A package manifest has `name`, `version`, `digest`, optional `targets`, optional `persona`, prompt payload locations, and abstract `requires` such as `workspace.read`, `workspace.patch`, `shell.execute`, `browser.navigate`, `brain.query`, and `schedule.manage`. A host verifies source trust and digest, maps only supported abstract requirements to host resources, then applies its own scope/approval/audit policy. Workspace and project overlays remain host-local and higher precedence than the shared immutable registry.

The default socket is `~/.oxi/brain/oxibrain.sock`. `oxibrain` owns creation, stale-socket removal after it proves no owner is live, lock coordination, and permissions. Clients discover it through `BrainClient` helpers and may explicitly override it. No client reads SQLite files directly.

The initial Foundation contract deliberately has no shared runtime crate. Each host parses the same schema and is checked against a shared fixture corpus. Extract a neutral implementation crate only after two hosts require the same non-trivial behavior; do not make `oxibrain` a configuration dependency of Oxicode or Oxios.

## Implementation Tasks

### 1. Publish the canonical Foundation and topology decision

**Files:**
- Modify: `doc/ARCHITECTURE.md`
- Rewrite: `doc/ECOSYSTEM.md`
- Modify: `doc/CONSUMPTION_CONTRACT.md`
- Create: `doc/spec/oxi-foundation-v1.md`
- Create: `doc/adr/ADR-007-oxi-foundation-contract.md`

- [ ] Bump the `ARCHITECTURE.md` version header and add the Foundation boundary to §4.3, §8, §15, §18, and §19 without altering P1–P11. State that the Brain daemon is the sole durable-memory data plane and that it is still additive: callers degrade when unavailable rather than creating an app-local durable fallback.
- [ ] In §8, preserve local GGUF as the default. Define profile-selected remote adapters as optional `LlmPort` implementations at the facade/CLI boundary; explicitly reject driving the Oxicode CLI for inference and reject a mandatory model gateway.
- [ ] In §13, distinguish `Brain::consolidate` from application note curation: it clusters ledger episodes, emits/cache-links `EpisodeKind::Derived`, preserves support and `Uncertainty`, and never extracts from or mutates a derived episode.
- [ ] Replace stale `doc/ECOSYSTEM.md` v0.2 topology and roadmap with the three-plane topology: Foundation contract, oxibrain durable data plane, Oxicode execution plane, Oxios orchestration/experience plane. Retain a cross-reference to the consumption contract rather than restating unstable API details.
- [ ] Define the on-disk schemas, Keychain locator abstraction, role-binding rules, digest/trust rules, host-capability rule, precedence, and invalid-profile rejection behavior in `doc/spec/oxi-foundation-v1.md`. Include JSON examples with redacted locators only.
- [ ] Record ADR-007: “Foundation v1 is a schema/protocol contract, not a mandatory runtime crate or model broker.” Explain the standalone constraint, why duplicate parse implementations are temporarily tolerated, and the extraction threshold for a neutral crate.
- [ ] Update `CONSUMPTION_CONTRACT.md` to list discovery and capability-handshake additions as additive client features and preserve existing auth-first-message and scope semantics.

**Acceptance:** Documentation has one topology, names `~/.oxi/brain/oxibrain.sock`, does not imply direct-store access, and has no contradiction between `ARCHITECTURE.md`, `ECOSYSTEM.md`, and the v1 schema.

### 2. Add stable daemon discovery and compatibility negotiation

**Files:**
- Modify: `crates/oxibrain-client/src/lib.rs`
- Create: `crates/oxibrain-client/src/discovery.rs`
- Create: `crates/oxibrain-client/src/protocol.rs`
- Modify: `crates/oxibrain-client/Cargo.toml`
- Modify: `crates/oxibrain-mcp/src/server.rs`
- Modify: `crates/oxibrain-cli/src/cmd/serve.rs`
- Create: `crates/oxibrain-client/tests/discovery.rs`
- Modify: `crates/oxibrain-client/tests/client_round_trip.rs`
- Modify: `crates/oxibrain-client/tests/degradation.rs`

- [ ] Introduce `BrainEndpoint` and `default_socket_path()` in `discovery.rs`. The default is `$OXIBRAIN_SOCKET` when set, otherwise `~/.oxi/brain/oxibrain.sock`; reject relative paths and create no directories from a client lookup.
- [ ] Add `BrainClient::connect_default()` and `BrainClient::connect_endpoint(&BrainEndpoint)` as additive helpers. Preserve `connect(path)` and `connect_with_token(path, token)` exactly for existing callers.
- [ ] Add versioned, serde-serializable `ClientHello`, `ServerInfo`, `BrainProtocolVersion`, and `BrainCapabilities` in `protocol.rs`. Include a minimum/maximum compatible protocol range, store format compatibility, supported role-independent client operations, and the daemon identity/version. Do not expose an API key or token in either direction.
- [ ] Implement the handshake as a transport-level JSON-RPC method available immediately after optional `auth`, before MCP tool routing. The server must reject incompatible versions with a typed response that names the supported range. Authentication and scope validation remain first for token-protected sockets.
- [ ] Make `serve --daemon` choose the discovery default when `--socket` is absent. Keep explicit `--socket` behavior unchanged. Ensure startup creates the parent with owner-only permissions, refuses a live competing owner, and removes a stale socket only after the existing lock/PID checks prove it is stale.
- [ ] Test default-path override and explicit-path precedence; authenticated and unauthenticated handshake success; incompatible-range rejection; a missing default socket failing fast; and an existing `Read`-only scope unable to escalate through discovery or handshake.

**Acceptance:** A host can connect by convention, receive an authenticated compatibility result, and degrade in less than one second when the daemon is absent. The MCP tool list remains fifteen.

### 3. Implement Foundation profile parsing and local-first LLM resolution

**Files:**
- Create: `crates/oxibrain-cli/src/foundation.rs`
- Modify: `crates/oxibrain-cli/src/llm.rs`
- Modify: `crates/oxibrain-cli/src/main.rs` or the existing command module index that exposes configuration errors
- Modify: `crates/oxibrain-ports/src/llm.rs`
- Modify: `crates/oxibrain-llm-http/src/anthropic.rs`
- Modify: the sibling OpenAI adapter in `crates/oxibrain-llm-http/src/`
- Create: `crates/oxibrain-cli/tests/foundation_profiles.rs`
- Modify: `crates/oxibrain-ports/tests/` or add focused LLM capability tests following the workspace convention

- [ ] Define schema-validated `FoundationProfiles`, `ProviderProfile`, `ProfileRole`, `SecretLocator`, and `DeclaredModelCapabilities` in the CLI boundary. Load only `~/.oxi/foundation/v1/profiles.json` (or an explicit `OXI_FOUNDATION_HOME` test/deployment override), reject unknown schema versions and malformed/duplicate profile IDs before a provider is created.
- [ ] Add a small `SecretResolver` trait at the CLI adapter boundary with a production OS-Keychain implementation and deterministic test implementation. The trait returns a secret only from a validated locator; it never returns a serialized credential and is never named from core/store/index.
- [ ] Extend `LlmCapabilities` additively with only truthfully reportable constraints needed for profile validation (for example, tool call / JSON Schema / grammar). Existing adapters must retain their current grammar and structured-output values and default safely for newly added fields.
- [ ] Refactor `resolve_provider` so resolution is: explicit CLI/environment override for automation; an allowed valid Foundation profile for the requested role; existing `ANTHROPIC_*`/`OPENAI_*` compatibility environment resolution; then local model. A missing/unavailable Keychain secret must report why that profile cannot run and fall through only where policy permits. It must never silently send extraction to a different remote provider.
- [ ] Keep `OXIBRAIN_LLM_PROVIDER=local` and no-key installation behavior intact. The model manifest remains under `~/.oxi/models` and model digest continues feeding `ExtractorConfig`/`ExtractorId`.
- [ ] Assert a profile is rejected if its declared output capabilities cannot satisfy the configured extraction mechanism; assert environment override precedence; Keychain absence behavior; profile role denial; local fallback; and that parsed profile JSON cannot contain credential-shaped fields.

**Acceptance:** Remote profiles are registered once without secrets on disk, model mechanism choice remains truthful, and an air-gapped default installation still extracts with the local model.

### 4. Bind package metadata without changing Brain authority

**Files:**
- Modify: `doc/spec/oxi-foundation-v1.md`
- Create: `crates/oxibrain-client/src/foundation_package.rs`
- Modify: `crates/oxibrain-client/src/lib.rs`
- Create: `crates/oxibrain-client/tests/foundation_packages.rs`
- Modify: `crates/oxibrain-mcp/src/server.rs` only if an existing non-tool metadata endpoint needs package provenance in a response

- [ ] Define typed package-lock and manifest reader types for the shared immutable registry: package identity, digest, source, trust state, targets, persona metadata, and abstract requirements. Do not execute package payloads in oxibrain.
- [ ] Offer read-only helpers that let a host inspect a resolved package and its digest after it has independently selected a compatible target. Do not add an MCP skill execution tool, a package installer, or automatic prompt injection to oxibrain.
- [ ] Add a `brain.query` abstract requirement mapping only to existing scoped Brain read operations. `brain.ingest`, `brain.declare`, `brain.retract`, and `brain.redact` remain distinct privileged capabilities; no package grants them by declaration.
- [ ] Test schema/version rejection, digest mismatch, target exclusion, unknown abstract requirement preservation, and that the client helper cannot modify the lockfile or bypass server `Scope` checks.

**Acceptance:** Brain participates in portable package discovery without becoming a package runtime or granting a caller more authority than its existing scope.

### 5. Preserve deterministic consolidation during shared use

**Files:**
- Modify: `crates/oxibrain/src/extraction.rs`
- Modify: `crates/oxibrain-store/src/consolidation.rs`
- Modify: focused consolidation tests in `crates/oxibrain/tests/` and/or `crates/oxibrain-store/tests/`
- Modify: `doc/ARCHITECTURE.md` §13 only if implementation exposed an unstated invariant

- [ ] Thread Foundation profile identity and model digest into the existing extractor configuration/provenance that already keys cached summaries. Do not use profile display names, Keychain locators, wall-clock values, or map iteration order in truth-half persisted identifiers.
- [ ] Keep `find_episode_clusters`/`hash_member_set`/checkpoint behavior deterministic. Use the selected `LlmPort` only after a checkpoint is established and never hold a store transaction across model or Keychain work.
- [ ] Require every newly produced derived episode to retain source links and computed `Uncertainty`. A profile failure may leave a resumable checkpoint but must not create an uncited summary or mutate source episodes.
- [ ] Add tests for profile identity changing cache provenance without changing truth-fold output, failure before cache write/resume, and full reproject equivalence after consolidation-backed derived episodes are present.

**Acceptance:** Shared model profiles affect optional ranking/summary provenance only; ledger truth remains replay-deterministic and consolidation never becomes user-note curation.

### 6. Execute repository gates and cross-host fixture checks

**Files:**
- Modify: `doc/spec/oxi-foundation-v1.md` fixture references as needed
- Create or update: schema fixtures under the existing relevant test fixture directory

- [ ] Add accepted and rejected Foundation profile/package fixture documents, including unknown schema version, duplicate ID, Keychain locator without secret, unsupported model capability, digest mismatch, and target mismatch.
- [ ] Run focused client, server auth/scope, profile resolution, package parser, and consolidation tests while implementing each task.
- [ ] Run final Rust gates:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1
```

- [ ] Perform an end-to-end smoke test: launch `oxibrain serve --daemon` without an explicit socket; connect with `BrainClient::connect_default`; complete authenticated compatibility negotiation; ingest/recall through existing scoped calls; stop the daemon; then verify a client returns the typed fast degradation error.

**Acceptance:** Foundation fixtures are portable for Oxicode and Oxios, all repository gates pass, default build remains standalone, and the actual daemon/client path works without a direct database connection.

## Out of Scope

- A mandatory shared `oxi` binary, model proxy, or long-lived gateway.
- Making Oxicode/Oxios dependencies of the default oxibrain build.
- Executing shared skills/personas in oxibrain.
- A second durable memory backend or an application-local fallback store.
- Treating `Brain::consolidate` as a note-writing, git-committing, or source-mutating workflow.
