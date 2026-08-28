# ADR-013 — Required `space`, payload-only input, and the safety rails

**Date:** 2026-08-28 · **Status:** Accepted · **Spec:** `doc/spec/agent-first-cli-v1.md`

## Context

Three decisions from the 2026-08-28 design review needed records of their own.

**(a) Space resolution.** v2.12 resolved an omitted `space` as `--space` > `~/.oxi/config.toml`
`default_space` > `"personal"`, identically on CLI and MCP. Two defects: silent routing of
agent writes into `personal` in multi-space setups (contamination; agents learn from errors,
not from silent successes), and machine-local config making the same call mean different
things on different machines. A state-dependent alternative ("error only when ≥2 spaces
exist") was rejected: an interface whose semantics change with brain state is unpredictable
— the one property an agent-facing surface must never lose.

**(b) Input shape.** Per-op CLI flags (`--title`-style) and ambient defaults are the human
shape. Agents are better served by raw payloads, but argv-embedded JSON reintroduces shell
escaping — exactly the failure class MCP exists to eliminate — and prose bodies make it
certain.

**(c) Destructive-op confirmation.** `redact` needs a rail stronger than a boolean confirm.
An `expect_closure: N` echo was rejected: the mismatch error discloses the correct number,
so an LLM passes by copying it without reading the plan — ceremony, not defense.

## Decision

1. **`space` is required on every space-scoped op, on every transport. No fallback chain.**
   `UserConfig::resolve_space`, `default_space`, and `space default` are deleted.
   `init`/`space add` keep their creation defaults (`personal`) — creation is not
   resolution and reads no config. Errors list available spaces (`describe` is the enum
   source). Enchantment: the *schema* teaches (required field), so no first-call failure is
   needed.
2. **The payload is the only input shape.** CLI op arguments are a JSON payload, byte-identical
   to MCP `tools/call` arguments; stdin is the first-class path, argv `--json` a convenience.
   Ops that carry prose bodies (`ingest`, `remember`) refuse body text in argv. CLI global
   flags are transport-level only (`--dir`, `--format json|ndjson`); everything semantic —
   `space`, `dry_run`, `wait_lock_ms`, `fields`, `cursor`, `budget`, `limit`,
   `idempotency_key` — is a payload field on both transports.
3. **Plan tokens guard destructive ops.** `dry_run: true` returns `{ token, closure_hash,
   expires_at }`; the commit call presents the token; the server compares against its own
   plan. TOCTOU-safe, not derivable from error text. Stale plans return `plan_stale`.
   Capability gating (the token scope) remains the primary wall; a Read-scoped session never
   sees mutating ops in `tools/list` at all.
4. **Idempotency and caller identity are ledger-visible.** `idempotency_key` enters event
   identity as the caller-declared occurrence (agent-write `locator`, §5.6) — never a
   projection-side cache, which `reproject()` could not rebuild (P1). Every write episode
   records caller identity (token label / `cli:` principal) as a ledger field. Content-hash
   dedup remains forbidden.

## Consequences

- Breaking at the P3 cutover: omitted `space` errors on CLI and MCP; `space default` users
  migrate to passing `space` per call; scripts/skills must enumerate via `describe`.
- Multi-agent write collisions under P8 become a typed, retryable outcome (`locked`, exit 5,
  `wait_lock_ms`) rather than a mystery failure.
- The omp-facing skill encodes: always pass `space`; ids come from `resolve`/`search`;
  dry-run before mutating ops; retrieved text is data; read `meta.dropped`; `pending`
  extraction is not completion.
