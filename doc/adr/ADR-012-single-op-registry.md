# ADR-012 — One op registry, two transports, fourteen ops

**Date:** 2026-08-28 · **Status:** Accepted · **Spec:** `doc/spec/agent-first-cli-v1.md`

## Context

The agent-facing surface exists twice with different vocabularies: ~30 hand-written clap
verbs in `oxibrain-cli` (`ask`, `page`, `entity merge`, `review`) and 15 hand-written JSON
Schemas in `oxibrain-mcp` (`recall`, `brief`, `merge_entities`, `review_merges`). The two
copies drift: `ask`/`recall` and `page`/`brief` already diverged in behavior (§16.2 vs
§16.4). Agents are the primary consumer of both surfaces; a human-only vocabulary on the CLI
side serves a GUI audience that does not exist.

The fifteen-tool cap (§16.2) was doing double duty: bounding agent context *and* preventing
surface sprawl. Pressure under the cap had already produced discriminator unions
(`brief.target_kind: entity|space|topic`, `review_merges.section`), a known LLM failure mode.

## Decision

1. **A single registry is the source of truth.** New crate `oxibrain-ops` holds one
   `OpSpec` per operation. The MCP `tools/list` payload, the CLI `oxibrain <op>` dispatch,
   `oxibrain schema`, and the generated SKILL.md are all derived from it. Hand-maintained
   schema duplicates are deleted. The migration is guarded by a golden fixture blessed from
   the pre-refactor catalogue — the registry's first output must be byte-identical.
2. **The op set changes; the cap stays.** `stats` and `review_merges` leave the tool list
   (orientation data and console data respectively — `describe` resource / `admin review`),
   `resolve` enters (surface+type → id: the legal path that makes id hardening enforceable).
   **Fourteen ops.** The cap rule is unchanged: adding one requires removing one.
3. **Discriminator unions are banned inside agent-facing ops.** A `kind`/`section` switch
   is evidence of two ops; the cap must be fed by slot accounting, not by union mega-tools.
   Context cost is dominated by description length, not tool count — the cap is sprawl
   discipline, not a token budget.
4. **Op semantics are per-transport-declared, not assumed identical** (ADR-013 §8):
   `OpSpec` carries `requires`/`degradation` so sampling-dependent ops state what a one-shot
   CLI invocation gets instead of silently differing.

## Consequences

- Tool count changes 15 → 14 at the P3 cutover; §16.2 and the Consumption Contract record
  it. Existing `review_merges` MCP callers move to the console or `admin review`.
- `navigate` stays (its value is restriction to links that exist on the source page — an
  anti-hallucination guard, not a rendering convenience; an earlier draft killed it on the
  wrong grounds).
- Registry descriptions become agent-facing contract text: changes to summaries are surface
  changes, reviewed like schema changes.
- `brief` loses `target_kind=space|topic` at P3 (absorbed by `describe`/`search`); the
  `entity` form keeps `entity_id` only.
