# ADR-007: Oxi Foundation v1 — Schema/Protocol Contract, Not a Mandatory Runtime Crate or Model Broker

- **Status:** Accepted
- **Date:** 2026-08-17
- **Supersedes:** none
- **Superseded by:** (none yet)

## Context

`oxibrain` is the durable-memory data plane for the oxi ecosystem. Three things need
to be coordinated across the consuming hosts (oxicode, oxios, oxiline, oximemo, and
external MCP clients):

1. **Provider / model profiles.** Which provider, which model, which Keychain-locator
   holds the secret, and which role the profile is permitted to satisfy.
2. **Foundation packages.** Which immutable packages a host has resolved, with their
   digests, sources, trust tier, targets, and abstract capabilities.
3. **Discovery.** Where the daemon is listening and how the client and the daemon
   negotiate schema version and supported features.

These three things are shared. The naive implementation is a shared Rust crate that
every host depends on. That has a known cost: the shared crate becomes a configuration
dependency of every host, the daemon becomes a transitive build dependency of hosts
that have no business linking it, and any change to the crate forces a synchronized
release across three or more repositories that evolve on different schedules.

The cross-cutting decision here is: what is the Oxi Foundation v1, exactly? Three
candidates were considered:

- **A.** A shared `oxi-foundation` crate every host links. Provides parser, types,
  `SecretResolver` trait, OS-Keychain adapters. Single source of truth, but couples
  every host's release cadence to the foundation crate, and makes `oxibrain`
  configuration visible to hosts that integrate only at the client level.
- **B.** The current document plus a JSON Schema and a cross-host fixture corpus.
  Each host parses the schema itself and is checked against the shared corpus. No
  shared runtime. Slow to converge if two hosts need the same non-trivial behavior,
  but never blocks a host release on another.
- **C.** A sidecar daemon (`oxi-foundationd`) that brokers profiles, secrets, and
  package loading for every host. Centralizes policy. Adds a second daemon, a second
  socket, and a single point of failure that did not exist before.

The standalone constraint from `ARCHITECTURE.md` §0 ("`oxibrain` is a standalone,
local-first knowledge and memory system") rules out **A**: a shared crate linking
`oxibrain-core` would violate the "no oxi-ecosystem crates required for a default
build" rule, and would push `oxibrain` into becoming a configuration dependency of
hosts (oxicode, oxios). The same rule rules out **C**: a Foundation daemon is a
second daemon, a second socket, and a second point of failure; "adversarial
remembering" is the rule the data plane exists to enforce, not weaken.

What remains is **B**.

## Decision

The Oxi Foundation v1 contract is a **versioned filesystem/protocol contract**,
frozen in `doc/spec/oxi-foundation-v1.md`. It consists of:

- Two on-disk files in `~/.oxi/foundation/v1/`: `profiles.json` and `packages.lock`,
  each with a `schema_version` literal. Both files are non-secret by construction;
  profiles carry a `{service, account}` Keychain locator and never a secret.
- A fixed set of role values (`memory.extract`, `memory.consolidate`,
  `coding.primary`, `assistant.general`) and a fixed set of abstract requirements
  (`workspace.read`, `workspace.patch`, `shell.execute`, `browser.navigate`,
  `brain.query`, `schedule.manage`).
- A fixed set of rejection rules: unknown `schema_version`, malformed JSON,
  duplicate profile `id`, unknown role, secret-shaped field in a profile, malformed
  `digest` in `packages.lock`, unknown abstract requirement.
- A cross-host fixture corpus at `tests/fixtures/oxi-foundation/v1/` that every
  host parses with the same expected outcome.
- An additive `oxibrain-client` surface (`default_socket_path`, `connect_default`,
  `connect_endpoint`, `BrainEndpoint`, `ClientHello`, `ServerInfo`) for daemon
  discovery and capability negotiation. The daemon's canonical default listening
  socket is `~/.oxi/brain/oxibrain.sock`, overridable via the `$OXIBRAIN_SOCKET`
  environment variable (`serve --daemon` binds the default when `--socket` is
  absent). The MCP tool surface stays at fifteen; the handshake rides the existing
  transport, not a sixteenth tool.

Three explicit non-decisions follow from this:

1. **No Foundation runtime crate.** Each host parses the schema itself. Parsing v1 is
   not non-trivial enough to demand a shared crate yet.
2. **No Foundation daemon.** The Foundation contract is not a process. There is no
   `oxi-foundationd`, no Foundation-side socket, no Foundation-side model broker.
3. **No Foundation-side inference router.** The data plane is `oxibrain`; profile
   selection is the host's call. Two hosts may hold the same `profiles.json` and
   select different engines at the facade. A profile is a policy document, not a
   routing key.

### Why duplicate parse implementations are temporarily tolerated

Three reasons the duplication is acceptable for v1:

- **Parsing v1 is small.** Two JSON shapes, one role enum, one requirement enum, a
  digest regex. There is no algorithm worth extracting yet; each host's parser is
  shorter than the build glue a shared crate would add.
- **The fixture corpus is the cross-host test.** If two hosts disagree on a fixture,
  the contract has drifted and the failure is visible in CI on both sides. The corpus
  replaces the type system a shared crate would have given.
- **The release cadence is currently solo.** A solo effort cannot sustain the
  coordination cost of a shared crate's semver discipline across three or four
  repositories on different schedules. Decoupling lets each repo ship when it is
  ready.

### Extraction threshold — when a neutral crate becomes correct

A neutral `oxi-foundation` crate (or `oxi-foundation-types`) becomes correct when
**two hosts require the same non-trivial behavior**. "Non-trivial" excludes the
two-shape parser, the role enum, the requirement enum, and the rejection rules
above. Concrete triggers:

- A third JSON shape is added (e.g. `~/.oxi/foundation/v2/packages.json`) and a
  second host's parser is reaching the same ~500 lines of validation logic.
- A `SecretResolver` implementation is shared by two hosts and the test
  determinism story is duplicated in three test harnesses.
- A digest-verification routine (sha256 + signature verification) is duplicated in
  two hosts and the same CVE patch needs to land in both.

Until one of these fires, the shared crate is overhead without payoff. When one
fires, the extracted crate is a leaf: it parses the schemas, exposes the enums,
provides the trait, and depends on nothing that would force a configuration link to
`oxibrain`. The `oxibrain` data plane continues to not depend on it; consumers that
want the helpers import them, consumers that do not still parse the schemas
themselves.

### Standalone constraint, restated

`oxibrain-core`, `oxibrain-store`, and `oxibrain-index` MUST NOT depend on any
Oxi Foundation runtime crate, on `oxicode-*`, on `oxios-*`, on provider SDKs, or
on the OS Keychain. The data plane takes an `LlmPort` that is already wired; it does
not know which Foundation profile produced it. A default build pulls zero
`oxios-*` or `oxicode-*` crates, and the local GGUF inference (`oxibrain-llm-local`)
remains the C2 default that works without keys or network access.

## Consequences

- **Single source of truth for the contract.** `doc/spec/oxi-foundation-v1.md` is the
  contract. `doc/adr/ADR-007-oxi-foundation-contract.md` is the rationale. Drift
  between them is a documentation bug to fix, not a design choice.
- **Cross-host testing lives in the fixture corpus.** Two hosts' CI must both parse
  every fixture in `tests/fixtures/oxi-foundation/v1/`. A host that passes the corpus
  but disagrees with another host's interpretation of an edge case has a bug.
- **A profile is a policy document, not a routing key.** Two hosts may hold the same
  `profiles.json` and select different engines. The contract does not centralize
  routing; the host does. This is the rule that keeps "every host must use the same
  model" from being a contract-level possibility.
- **The MCP tool surface stays at fifteen.** Discovery and capability negotiation
  ride the additive transport handshake in `oxibrain-client` 0.3.x. Adding a
  sixteenth MCP tool to expose Foundation metadata would be a contract violation.
- **Auth-first-message and `Scope`/`Capability` semantics are preserved.** The
  existing token-before-payload rule and the scope model in `ARCHITECTURE.md`
  §15.1–§15.2 are unchanged by the Foundation contract. `ClientHello` and
  `ServerInfo` carry metadata only.
- **Environment variables remain an explicit override.** `ANTHROPIC_*` / `OPENAI_*`
  continue to work; they are the development/automation path, not the Foundation
  path. The Foundation contract does not retire them.
- **`oxibrain-client@0.2.0` keeps working unchanged.** The additive surface
  (`default_socket_path`, `connect_default`, `connect_endpoint`, `BrainEndpoint`,
  `ClientHello`, `ServerInfo`) lands in `oxibrain-client@0.3.x`. Hosts pinned to
  0.2.0 do not need to migrate.
- **Fixtures must be kept in sync across hosts.** oxicode already mirrors the corpus
  at the same path; the oxibrain repo carries the canonical tree. When a fixture is
  added or amended in oxibrain, the change must be mirrored in oxicode. The reverse
  is also true for fixtures oxicode originates.

## References

- `doc/ARCHITECTURE.md` §4.3, §8.6, §15.7, §19.3 — boundary, daemon-as-data-plane,
  profile resolution, and the Foundation plane.
- `doc/ECOSYSTEM.md` v1.0 — three-plane topology, contracts C1–C8.
- `doc/CONSUMPTION_CONTRACT.md` §1.1 — additive `oxibrain-client` surface.
- `doc/spec/oxi-foundation-v1.md` — the frozen contract.
- `tests/fixtures/oxi-foundation/v1/` — cross-host fixture corpus.
- ADR-002 — `oxios` fallback decision (the brain is additive, never load-bearing).
- ADR-003 — local inference engine (C2 default).
- ADR-006 — quote-based mention evidence (recent ADR precedent for the format).
