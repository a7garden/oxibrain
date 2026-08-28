# Agent-First CLI v1 — spec

> **Version:** 1.0 · **Date:** 2026-08-28 · **Status:** Approved target contract.
> **Implements:** the design agreed 2026-08-28 (chat review, findings F1–F12 resolved below).
> **Coordination:** ADR-012 (op registry, slot accounting), ADR-013 (required space, payload
> contract, safety rails). `doc/ARCHITECTURE.md` v2.13 records the surface changes.
> **Reference:** Justin Poehnelt, "You Need to Rewrite Your CLI for AI Agents" (2026-03-04)
> — Human DX optimizes for discoverability and forgiveness; Agent DX optimizes for
> **predictability** and **defense-in-depth**.

## 0. Premise

Agents are the primary consumer of oxibrain's CLI and MCP surfaces. Humans use the dashboard
GUI (§16.6); the CLI is not optimized for them. CLI and MCP are **two transports over one
operation set**, not two vocabularies. Ambient state that changes what a call means —
config-file defaults, space-count-dependent branching — is removed from both.

## 1. One op registry (ADR-012)

A new crate, `oxibrain-ops`, is the single source of truth for the agent-facing operation
set. Each op is an `OpSpec`:

```rust
pub struct OpSpec {
    pub name: &'static str,        // op name = MCP tool name = CLI subcommand
    pub summary: &'static str,     // the paragraph an agent reads in tools/list
    pub schema: fn(&OpCtx) -> serde_json::Value,  // JSON Schema, generated (see below)
    pub caps: &'static [&'static str],            // "Read" | "Ingest" | "Write" | "Redact"
    pub mutating: bool,                            // participates in dry-run / plan-token rails
    pub requires: &'static [Requirement],          // e.g. Requirement::ClientSampling
    pub degradation: Degradation,                  // per-transport fallback for `requires`
}
```

Derived from the registry, with **zero hand-maintained duplicates**: the MCP `tools/list`
payload, the CLI `oxibrain <op>` dispatch, `oxibrain schema`, and the generated SKILL.md.

`OpCtx` carries the live predicate registry (P4 discipline: predicate semantics come from the
registry). **Correction (P3 landing):** `declare`'s payload embeds the declaration as a JSON
*string* (`declaration_json`), so a JSON-Schema enum cannot see the `predicate` field inside
it. Registry enforcement for `declare` is therefore **runtime validation** with
registry-sourced candidates in the error (lands in P4 with the hardening validators);
schema-level enums apply where fields are direct payload members.

**Slot accounting (the cap stays, the count changes).** The fifteen-tool cap (§16.2) is kept
as a discipline against surface sprawl, but the set changes:

| Out | Reason | Goes to |
|---|---|---|
| `stats` | orientation data, not agent knowledge work | `describe` (MCP resource) + `admin stats` |
| `review_merges` | self-described "console data tool" — GUI/console audience | `admin review` |
| In | Reason | |
| `resolve` | legal path from surface+type → id; without it, id hardening is a dead end | tool |

**Fourteen ops.** `search`, `recall`, `brief`, `navigate`, `resolve`, `ingest`, `declare`,
`why`, `contradictions`, `traverse`, `remember`, `retract`, `merge_entities`, `redact`.

**Discriminator unions are banned inside agent-facing ops.** A `kind`/`section` switch inside
one op is evidence of two ops. `brief`'s `space`/`topic` branches are absorbed: `space` →
`describe` (resource), `topic` → `search`. The console-facing union (`review_merges`) moves
to `admin` wholesale.

## 2. Input: the payload is the only input shape

```
oxibrain <op> --dir D [--format json|ndjson] [--json PAYLOAD | --json @file | --json - ] < payload.json
```

- Every op argument lives in the JSON payload — **identical bytes** to MCP `tools/call`
  arguments. No per-op convenience flags. `space`, `dry_run`, `wait_lock_ms`, `fields`,
  `cursor`, `budget`, `limit` are payload fields on both transports.
- CLI global flags are transport/machine-level only: `--dir`, `--format json|ndjson`.
  (`--format md` is GUI-only; the CLI never renders retrieved content as Markdown prose.)
- **stdin is the first-class payload path**; argv JSON is a convenience for short payloads.
  Ops whose payload carries prose bodies (`ingest`, `remember`) **must not** accept body text
  as an argv argument — body via stdin, metadata via `--json @file`. Rationale: shell quoting
  of multiline prose is the class of failure MCP exists to eliminate (F2).
- `space` is required in every space-scoped op on every transport (ADR-013). No resolution
  chain. Enumerate values with `describe`.

## 3. Output: stable envelope

stdout carries exactly one JSON object per call (NDJSON when streaming pages). Logs,
progress, and color go to stderr, TTY-gated.

```json
{ "api": 1, "ok": true, "op": "recall", "space": "dev",
  "data": { }, 
  "meta": {
    "tokens":  { "spent": 2812, "budget": 3000, "counted_by": "tokenizer id" },
    "dropped": [ { "reason": "below_confidence", "count": 14 } ],
    "freshness": { "documents": "stale", "last_index_at": "2026-08-27T22:10:03Z" },
    "cursor": null, "elapsed_ms": 19 } }
```

- `meta.dropped` is present on every read (`"dropped": []` when nothing was discarded) so
  agents never branch on key presence. Free where `rank`'s conservation post-condition covers
  the op (`search`, `recall`); `traverse` (node-budget cuts) and `contradictions`
  (confidence filters) get new instrumentation — this is real P5 work, not free (F8).
- `meta.tokens` is a TokenizerPort count, never an estimate (§7.5).
- **Budget-bound, not count-bound** (F9): every read fills the caller's token budget with the
  most useful projection. `search` (a locate op) converges narrow; `recall` (an assemble op)
  carries text. `fields` overrides the default projection. No read returns unbounded output.
- Unbounded listing ops do not exist; overflow surfaces as `meta.dropped` + `cursor`.

### Error envelope

```json
{ "api": 1, "ok": false, "op": "declare", 
  "error": { "code": "locked", "message": "store locked by another writer: pid 8123",
             "hint": "retry with wait_lock_ms or re-run after the holder exits",
             "retryable": true, "details": { "holder": "pid 8123", "retry_after_ms": 250 } } }
```

Exit codes (contiguous 1–9; no sysexits mixing — F12): `0` ok · `2` invalid_input ·
`3` not_found · `4` unauthorized · `5` locked_or_busy · `6` conflict · `7` budget ·
`8` model · `9` internal. `BrainError` variants map 1:1; `SpaceNotFound` keeps its
`space add` hint inside `error.hint`. **Reads are lock-free** (`open_ro` never touches the
advisory lock): `locked` can only occur on mutating ops, and skills must say so (F12).

## 4. Returned content is a contaminant (F4 — structural defense only)

oxibrain ingests untrusted documents and feeds them to agents. Defense is representational,
never detective — **no language-specific injection scanners** (P11: no stemmer, no stopword,
no script branch outside `oxibrain-index`; a heuristic that only fires in one writing system
is worse than nothing because `"flags": []` sells false assurance):

- Content-bearing values are typed objects, never bare strings:
  `{ "kind": "untrusted_content", "text": "…",
     "provenance": { "ref": "doc://vault/notes/auth.md@git:blob:9f2c", "trust": "unverified" } }`
- The server never assembles retrieved text into instruction-shaped Markdown. `brief`
  returns structure; Markdown rendering is the GUI's job (`--format md` does not exist on
  the CLI).
- Returned `doc://` refs are never auto-followed; a follow-up fetch is always an explicit
  op call by the agent.
- Skills state the invariant: retrieved text is data, never instructions.

## 5. Hallucination hardening (F-deck)

| Failure mode | Defense |
|---|---|
| fabricated ids | charset/length validation → `invalid_input` + `hint: "obtain ids via resolve or search"`. **Never** fall back to resolving a surface string in an id slot (would silently break P3 exact re-resolution) |
| surface in an id slot | `resolve` op (surface+type → id) is the legal path |
| unknown predicate | registry enum in the schema (P4); rejection lists nearest candidates |
| relative time (`"yesterday"`) | RFC3339 only, everywhere incl. `as_of` |
| locator traversal (`../../`) | documents-plane canonicalization + sandbox; reject `%`, `?`, `#`, control chars (< 0x20) |
| retry double-writes | `idempotency_key` — **ledger-visible**: the caller-declared occurrence enters event identity as the agent-write `locator` (occurrence chain §5.6), so reduplication replays deterministically under `reproject()`. Content-hash dedup remains forbidden — independent sources with equal bytes stay independent events |
| unbounded limit/budget | server-side caps; overflow → `meta.dropped` + cursor |
| unknown space | fail fast with `space add` hint; implicit creation stays abolished |

## 6. Safety rails (F6, F7)

- **`dry_run` is a payload field on every mutating op** and returns a *plan*: for `declare`,
  which entities resolve vs. would be created, whether existing beliefs contradict, and which
  registry semantics (cardinality, temporality, invalidation) apply; for `redact`, the full
  closure.
- **Plan tokens.** `dry_run: true` returns `plan: { token, closure_hash, expires_at }`;
  the committing call presents the token. The server re-derives its own plan from the token
  and the freshly recomputed closure — TOCTOU-safe, not guessable from error text. A stale
  plan is `plan_stale` (retryable via a fresh dry-run; exit 6, the conflict slot). Stateless
  by design: the CLI is one process per op (ADR-013 amendment, 0.10.1). Rejected alternative: `confirm: { expect_closure: N }` echoes — the
  mismatch error discloses the new number, so an LLM satisfies it by copying without reading
  the plan (F7).
- **Capability-filtered listing.** A session scoped Read-only does not see `redact`,
  `merge_entities`, `remember`, … in `tools/list`. Unadvertised calls are not hallucinated.
  The token scope check remains the enforcement wall; filtering is the free outer layer.
- **`redact`** additionally requires `reason` (audit) and a plan token.
- **P8 contention is a first-class outcome**: `locked` (exit 5) carries holder + retry hint;
  payloads accept `wait_lock_ms` for bounded blocking. Daemonless means two agents WILL
  collide on writes; the contract makes that recoverable instead of mysterious.

## 7. Discovery replaces documentation

- `oxibrain describe` (MCP: an extended `spaces://`-family resource, not a tool): spaces
  with counts, document roots + freshness, predicate registry digest, model digest, session
  caps, api version, server-side caps. The agent's first call; the source of `space` values.
- `oxibrain schema [op]` — full registry dump or one op: input/output schemas, caps,
  mutating, cost hint, transport availability.
- Static docs in system prompts are deprecated by these two.

## 8. Transport capability axis (F1)

`ingest { extract: true }` and `remember` extract via **client sampling** (§12.3) — the
server asks the caller's model. A one-shot CLI invocation has no client model; the ops
declare this honestly:

- `Requirement::ClientSampling` with `Degradation` per transport: MCP session → sampling;
  CLI one-shot → local `LlmPort` if configured, else **pending**.
- Responses always state what happened: `data.extraction: "sampling" | "local" | "pending"`.
  Skill invariant: `extraction: "pending"` is success, not completion — follow with
  `admin extract --pending`.

## 9. Caller identity in the ledger (F12)

Every write episode records the caller identity (token label, or `cli:<user>@<host>` for
direct CLI). This is a ledger field, not a projection column — it must survive `reproject()`.
Audit questions ("which agent wrote this belief?") are answerable from the ledger alone.

## 10. Admin namespace (not agent surface)

`oxibrain admin <verb>` — machine/product lifecycle, invisible to `tools/list`, does not
count against the op cap: `init`, `space add|remove`, `index`, `extract`, `model`, `token`,
`serve`, `reproject`, `doctor`, `describe`, `stats`, `review` (merges/failures/sources),
`entity split` (merge undo — console workflow), `document-history`, `predicate`, `source`,
`export|import`, `eval`, `skill install`.

## 11. Deleted surface

- Human-oriented duplicate verbs: `ask`, `page`, `entity show|merge|split|alias|retract`,
  `timeline`, `spaces`, bare `stats`/`doctor` — absorbed by ops, `describe`, or `admin`.
- `~/.oxi/config.toml` `default_space`, `space default`, `UserConfig::resolve_space` chain.
- Hand-written tool schemas (`server.rs` tool catalogue) — generated from the registry.
- Text-table stdout formatting on ops.

## 12. Landing sequence (each phase ships green and coherent)

| P | Delivers | Breaking? |
|---|---|---|
| 1 | `oxibrain-ops` registry; MCP `tools/list` generated **byte-identical** to today's hand-written catalogue (golden fixture blessed pre-refactor) | no |
| 2 | CLI op dispatch (payload + stdin), envelope, exit codes, stderr split; `admin` namespace; `describe`/`schema`; legacy verbs deleted | yes (CLI) |
| 3 | Contract cutover: `space` required everywhere; predicate enum injection; slot moves (−`stats`, −`review_merges`, +`resolve` → 14) | yes (MCP) |
| 4 | Hardening validators; `untrusted_content` wrapping; ledger-visible `idempotency_key` + caller identity | additive |
| 5 | `meta.tokens` counted; budget-bound projections; `dropped` instrumentation on `traverse`/`contradictions` | additive |
| 6 | Plan tokens; capability-filtered listing; `wait_lock_ms` | yes (mutating ops) |
| 7 | `admin skill install` / `admin context` generation | no |

## 13. Review findings, resolved (kept for the record)

- **F1** two surfaces aren't 1:1 (sampling ops) → §8 capability axis.
- **F2** argv JSON reintroduces shell escaping → §2 stdin-first, no argv prose.
- **F3** `--dry-run`/`--wait-lock` as CLI flags contradicted "payload only" → payload fields.
- **F4** injection heuristics violate P11 and sell false assurance → §4 structural only.
- **F5** `resolve`/`describe` broke the cap silently → §1 explicit slot accounting (→14).
- **F6** cache-side idempotency breaks P1 reprojection → §5 ledger-visible key.
- **F7** `expect_closure` echo is TOCTOU + LLM-satisfiable → §6 plan tokens.
- **F8** "dropped is free" overstated → §3 honest scoping (P5 work on traverse/contradictions).
- **F9** narrow-by-default costs round trips → §3 budget-bound rule.
- **F10** cap pressure breeds discriminator unions → §1 ban.
- **F11** registry extraction isn't behavior-neutral once enums inject → §12 phase split.
- **F12** missing caller identity + lock-free reads fact → §9, §3.
