# ADR-002: C1 Fallback — No Local Recall Cache for v1

> **Note (2026-08-13):** `DESIGN §n` references below use `DESIGN.md` v1.0 numbering.
> That file is now `doc/ARCHITECTURE.md` v2.0 and its sections were renumbered. This ADR is a
> historical record and is left as written.

- **Status:** Decided
- **Date:** 2026-08-12
- **Supersedes:** none
- **Superseded by:** (none yet)
- **Resolves:** DESIGN §16.1 open question, §20 item 6

## Context

DESIGN §16.1 states the ecosystem contract:

> **The brain is additive, never load-bearing.** With the daemon down, every
> consuming app retains its primary function — oximemo captures to files,
> oxiline runs routines, oxios agents execute.

It then names the weakness honestly:

> After M5 it [oxios] has no memory code of its own, so a brain outage leaves
> its agents with *no* memory rather than degraded memory. Agents still run,
> so the letter of the contract holds — but oxios is the one consumer that may
> need a small local recall cache to satisfy its spirit. That is an M5 decision,
> recorded here so it is made deliberately rather than discovered.

DESIGN §20 item 6 defers the decision to M5.

## Decision

**No local recall cache for v1. Agents run without memory during a brain
outage.**

### Rationale

1. **Embedded mode is the default and has no C1 risk.** oxibrain's primary
   topology is embedded — the engine runs in-process inside the consuming
   application. There is no daemon to go down, no socket to fail, no separate
   process. In embedded mode the brain is always available as long as the
   application itself is running.

2. **Daemon mode is opt-in for multi-app scenarios.** The user who runs
   `oxibrain serve --daemon` has chosen to centralize the brain across
   multiple apps. This is a deliberate trade-off: better cross-app sharing in
   exchange for a single point of failure. The user owns that trade-off.

3. **A cache adds complexity for unmeasured benefit.** A local recall cache
   needs: a second store (in-memory or file-backed), sync logic to keep it
   fresh, eviction policy, and reconciliation on reconnect. All of this for a
   scenario (daemon outage during active agent use) that we have no real-world
   frequency data for. The cost is concrete; the benefit is hypothetical.

4. **The letter of the contract holds.** Agents execute without memory — they
   don't crash, don't hang, don't corrupt state. They are less useful, which
   is the expected degradation.

5. **Reversible.** If production data shows brain outages are frequent enough
   to harm the agent experience, a cache layer can be added in a point
   release without architectural changes. The `Brain` facade already exposes
   `assemble_context`, `query`, and `beliefs` — a cache is a read-through
   wrapper around these, not a redesign.

## What this means for oxios-kernel

After M5 migration:

- oxios-kernel calls `Brain::*` methods for all memory operations.
- If the brain is unreachable (daemon down), `Brain::*` calls return errors.
- oxios-kernel must handle these errors gracefully: log a warning, skip the
  memory operation, and continue the agent's primary function.
- No fallback store, no retry loop, no silent caching.

## Revisit trigger

When we have real outage-frequency data from production usage showing that
brain unavailability during active agent sessions is common enough to degrade
the user experience. At that point, option (a) from the design — "oxios-kernel
ships a minimal cache (last-N sessions)" — becomes the natural enhancement.

---

End of ADR-002. The C1 fallback question is resolved: no cache for v1, agents
degrade gracefully without memory, revisit with data.
