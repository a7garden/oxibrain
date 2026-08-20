# ADR-009: `Brain` remote topology deferred; `oxibrain-client` is the remote surface

**Status:** Accepted · **Date:** 2026-08-20

## Context

`ARCHITECTURE.md` §16.1 (through v2.9) promises: "`Brain` is one trait in
both modes: a consumer changes topology by changing one line" — i.e.
`Brain::open` (embedded) and `Brain::connect` (daemon) as one surface.
Implementation reality: the facade `Brain` has `open`/`open_ro` only; the
remote path is the separate `oxibrain-client::BrainClient`; transports live in
`oxibrain-mcp`. A gap-fix session (2026-08-20) measured the cost of closing
this literally and had to choose.

## Measured constraints

`Brain` (crates/oxibrain/src/lib.rs) holds engine-internal state:
`Arc<StoreHandle>` (single writer actor, P8), `Arc<dyn ClockPort>`,
`Option<Arc<dyn LlmPort>>`, `Arc<dyn TokenizerPort>`,
`Option<Arc<dyn EmbeddingPort>>`, and `Arc<Mutex<ResolutionCache>>`.
Methods `with_llm` / `extract_one_with` accept LLM trait objects — values
that cannot cross a process boundary by construction.

## Options considered

1. **Enum inner** (`Brain { inner: Embedded | Remote(BrainClient) }`):
   every one of ~40 inherent methods gains a remote arm with a JSON→typed
   mapping; LLM-injecting methods cannot be implemented remotely, so the
   "one surface" would contain methods that fail at runtime in one mode.
   Large diff, permanent per-method branching cost, no current consumer.
2. **Trait extraction** (`trait Brain`, `EmbeddedBrain`/`RemoteBrain` impls):
   `compat.rs` (the stable-surface compile test behind
   `doc/CONSUMPTION_CONTRACT.md`) references `Brain::method` as inherent
   function items; a trait split rewrites the compatibility contract for
   every consumer to gain… the same unspeakable-method hole as (1).
3. **Defer; align the document with the architecture that shipped.**
   ECOSYSTEM.md C6 already routes every ecosystem consumer through
   `oxibrain-client` ("integration is a client dependency, never a fork");
   nobody links the facade crate for remote use today.

## Decision

Option 3. The two typed surfaces are:

- **Embedded:** `Brain` (the `oxibrain` facade crate) — full API including
  port injection. For in-process embedders (the CLI, tests, future apps
  owning their store).
- **Remote:** `oxibrain-client::BrainClient` — thin, semver'd, per C6.
  First-party data operations ride the native JSON-RPC layer (as
  `handshake`, `reproject`, and now `spaces/list` do), NOT the fifteen-tool
  MCP surface, which remains agent-facing.

§16.1 is revised accordingly (v2.10). Unification is **post-v1**; the
trigger to revisit: a real consumer that must switch topology at runtime
without changing call-site types. If that consumer appears, option (2) with
a `trait Brain` in `oxibrain-ports` is the honest shape — it requires a
CONSUMPTION_CONTRACT major version, which is exactly what the defer avoids
buying today.

## Consequences

- No code is added to pretend parity that cannot exist (LLM injection).
- The first-party/agent split becomes explicit: agent surface = fifteen MCP
  tools; app surface = `BrainClient` + native RPC methods. New first-party
  needs extend the native RPC layer additively (MCP tool count unaffected).
- The doc-implementation mismatch is closed by amending the doc, which is
  the cheap side of the contract — and the side that was wrong.
