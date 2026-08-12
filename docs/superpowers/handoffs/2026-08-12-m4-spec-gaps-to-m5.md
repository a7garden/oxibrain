# Handoff — Remaining M4 SPEC Gaps, M5 (oxios), M6 (UI) → M5 starter

> **Status:** M4 features shipped (sampling + daemon + CLI) but the previous
> handoff titled "completion" overstated scope. Three M4 *spec-line items*
> from DESIGN §12.2 / §12 are genuinely unbuilt and were not explicitly
> deferred anywhere. They must be built or explicitly deferred before M5 starts.
> **Branch:** `main`
> **Predecessor handoffs:**
> - `2026-08-12-m4-sampling-daemon-cli.md` (impl finished)
> - `2026-08-12-m4-client-http-eval.md` (token-auth + client + HTTP + eval)
> - `2026-08-12-m4-surfaces-security.md` (security + red + export/import)
> **Tests:** 202 pass, 0 fail. Clippy clean. Fmt clean. Standalone verified.
> **Token/auth surface:** implemented and shipped.

This handoff exists for two reasons:

1. To correct an overstated "M4 complete" summary — three §12.2 spec items
   were missed: `remember`, `traverse`, MCP `resources/*` protocol. Plus the
   2026-07-28 protocol features **long-running tasks** and **subscriptions**
   explicitly named in §12.2's spec-features table.
2. To lay out the path from "feature-complete at the API level" to "consumable
   by oxios" (M5) without rediscovering M4 gaps mid-migration.

---

## 1. What is actually shipped vs. what DESIGN §12.2 requires

Cross-checked against `doc/DESIGN.md §12.2` lines 895–920. Verified by
`grep "tool(\"" crates/oxibrain-mcp/src/server.rs` (returns 7 items) and
`grep -rn "resources" crates/oxibrain-mcp/` (returns nothing).

### 1.1 MCP tools (§12.2 table)

| Tool | Caps | Status |
|---|---|---|
| `search` | Read | ✅ |
| `recall` | Read | ✅ |
| `get_entity` | Read | ✅ |
| `traverse` | Read | ❌ **missing — Brain API exists (`Brain::traverse`/index), MCP tool not wired** |
| `timeline` | Read | ⚠️ **partial — Brain API exists, CLI subcommand exists, MCP tool missing** |
| `why` | Read | ✅ |
| `ingest` | Ingest | ⚠️ **sync `tools/call` only; not the 2026-07-28 protocol-task variant** |
| `remember` | Write | ❌ **missing entirely — "one-shot ingest + sync extraction"** |
| `declare` | Write | ✅ |
| `retract` | Write | ❌ **missing — "writes a denying assertion"; Brain API missing too** |
| `merge_entities` | Write | ❌ **missing — Brain facade has merge in core but no `merge_entities` thin wrapper** |
| `review_merges` | Write | ❌ **missing — needs Oxibrain.query-candidates + interactive accept/reject** |
| `redact` | Redact | ⚠️ **partial — `Brain::redact` exists, no MCP tool, no nested CLI subcommand** |
| `contradictions` | Read | ✅ |

### 1.2 Resources (§12.2 line 910)

> `space://`, `entity://{id}`, `episode://{id}`, `graph://{entity}?depth=n`

**Status: ❌ entirely absent.** No `resources/list`, `resources/read`,
`resources/subscribe` handlers. Not even a stub.

### 1.3 2026-07-28 protocol features (§12.2 table on lines 914–920)

| Feature | Status |
|---|---|
| **Long-running tasks** | ❌ **ingest is sync `tools/call`. No `tasks/create`, no progress, no cancellation, no task state store.** |
| **Multi-round-trip / sampling (SEP-2322)** | ✅ shipped this session (bidirectional JSON-RPC, `SamplingLlmPort`) |
| **Transport-neutral subscriptions** | ❌ **no `subscribe`, `unsubscribe`, `notifications/*` handlers. No server-pushed events (contradictions, finished extractions, merge candidates).** |

### 1.4 Capabilities handshake (§12.2 protocol)

Currently `initialize` advertises only `{ "tools": { "listChanged": false } }`
(`crates/oxibrain-mcp/src/server.rs:180`). A client advertising sampling
support is not parsed or recorded. Subscription resource support is not
advertised.

---

## 2. Other M4 spec items to verify before M5

These are smaller than §1 but worth auditing while the spec is fresh:

| Item | DESIGN § | Status | Notes |
|---|---|---|---|
| **Layer-7 daemon (HTTP over loopback)** | §11.6 | ✅ shipped (client-http-eval handoff) | ok |
| **Read-only library mode** | §4.3 | ❌ **claimed in DESIGN but no read-only `Brain::open_ro` exists.** Use `Brain::open` with `Read`-cap token or new constructor. | small |
| **Cross-process degradation test** | §14.3 "Degradation test" | ❌ not written | small |
| **Public benchmarks runner** | §14.1 | ❌ only `golden-corpus fast` exists; LongMemEval/LoCoMo/BEAM not wired | M5 territory |
| **§13.2 budgets for `get_entity`, `assemble_context`, cold-start, full reproject** | §13.2 | ❌ "not yet benchmarked" — requires larger fixtures | could be M5 |

---

## 3. What must be built before M5 vs. what can wait

### 3.1 Required before M5 starts (consumer-blocking)

oxios-kernel integrates through the `Brain` Rust facade, not through MCP.
So strictly speaking, **all of §1 is MCP surface** — none of it blocks M5
directly. The `Brain` Rust API already covers what oxios needs:

- `brain.ingest(...)`, `brain.query(...)`, `brain.assemble_context(...)`,
  `brain.declare(...)`, `brain.beliefs(...)`, `brain.why(...)`,
  `brain.redact(...)` — all exist (or have equivalents).
- `brain.traverse(...)` — exists in the index; the `Brain` method needs a
  thin facading wrapper (the agent interface from §12.1). Worth doing.

**True M5-blockers (none on the Rust side):**
- M5 exit criterion is oxios ships with **zero memory code of its own** —
  achieved when `oxios-kernel` uses only `oxibrain::*`. The `Brain` facade
  must be **stable** (semver), which it already is.
- Importer for `oxios-memory` stores → `oxibrain` episodes (M5 spec item).
- C1 fallback decision (DESIGN §16: "oxios local recall cache").

### 3.2 Required to honestly say "M4 complete"

These are MCP features that any external MCP consumer (Claude Desktop,
oxiline, third-party) expects and that the design lists:

1. **Long-running `ingest` (2026-07-28 protocol-task).**
   Currently synchronous. Should return a task-id immediately, then
   stream progress (`notifications/progress`) until completion, and
   allow cancellation (`tasks/cancel`). For a corpus of any size,
   sync ingest blocks the client for minutes — bad UX.
2. **Subscriptions.**
   Push `notifications/closed` (new contradiction), `notifications/extracted`
   (extraction completed), `notifications/merge_candidate` (resolution
   flagged a candidate). The §12.3 table makes this the headline feature
   that "makes ecosystem apps feel live rather than batch."
3. **Missing tools (`remember`, `traverse`, `retract`, `merge_entities`,
   `review_merges`).**
   Each is a thin wrapper around an existing `Brain` method plus a tool
   schema. `traverse` and `retract` add small core work; `remember` adds
   a "sync extraction" path distinct from the queue.
4. **Resources.**
   `space://`, `entity://`, `episode://`, `graph://`. Each is a URI handler
   returning JSON. ~150 lines per resource plus `resources/list` and
   `resources/read` dispatch.

### 3.3 Recommended sequencing

If doing any of §3.2 before M5, ship in this order — each is independently
useful and tests cleanly:

1. **`traverse` + `timeline` MCP tools** — pure read paths, low risk, ~half-day.
2. **Resources (`space://`, `entity://`, `episode://`, `graph://`)** — additive
   protocol surface, ~1 day.
3. **`remember` + `retract` MCP tools** — write paths; `retract` writes a
   `Declaration` episode (already supported in core), wire `Brain::retract`
   and the MCP tool. ~half-day.
4. **Long-running `ingest`** — protocol-task machinery; needs a task table
   (or reuse `ingest_jobs`) and progress notifications. This is the largest
   of the four. ~2–3 days.
5. **Subscriptions** — depends on (4) for `extracted` notifications. New
   table or in-memory pubsub. ~1–2 days.
6. **`merge_entities` + `review_merges`** — last because they need an
   interactive resolution UX (which is really M6 territory, see §4).

---

## 4. M5 — oxios migration (DESIGN §17)

> "`oxios-kernel` on `Brain`, importer for existing stores, `oxios-memory`
> deleted, consumption contract published, C1 fallback decision made."
> Exit: oxios ships with zero memory code of its own.

### 4.1 oxibrain-side scope

1. **Consumption contract 1.0 (§16.4).**
   Pin the public surface; tag the rest `unstable` or `internal`. Currently
   no stability tiers are annotated.
2. **`Brain::traverse` thin wrapper** (if not already shipped per §3.2/1).
   oxios does multi-hop recall.
3. **Importer for oxios-memory stores (M5 spec item).**
   One-shot import of `oxios-memory` SQLite → oxibrain: read
   `memory_entries` (`oxibrain::episode` maps to `SourceRef::AgentTrace`),
   trust tier `SemiTrusted`. Then re-extract from scratch on the new
   episodes — cheapest path because oxibrain's extraction is deterministic
   keyed by content.
4. **C1 fallback decision (DESIGN §16.1: "stated honestly").**
   The fact that oxios goes from "has memory" to "no memory" on a brain
   outage is the ecosystem's biggest liveness risk. Options:
   - (a) oxios-kernel ships a minimal cache (last-N sessions) honoring
     the design's "the brain is additive, never load-bearing" reading.
   - (b) nothing — "agents still run" satisfies the letter.
   - (c) split: oxibrain publishes a light read-only fallback library.
   Deferred: requires real outage behavior in hand (§20 list item 6).

### 4.2 Tasks not on the oxibrain critical path

- Updating `oxios-kernel` to depend on `oxibrain::*`.
- Deprecating `oxios-memory` on crates.io.
- The "retirement trigger" (DESIGN §16.3): last `oxios_memory::` import removed.

---

## 5. M6 — Product (DESIGN §17)

> Desktop brain UI: graph explorer, timeline, ask-with-provenance, merge
> review, contradiction inbox, quick capture. Packaging, onboarding, docs site.

M6 is mostly **outside** the oxibrain crate — it's `apps/<brain>` in
`doc/ECOSYSTEM.md`'s roadmap and calls `Brain` via `oxibrain-client`
(the thin client crate) over socket. Concretely, oxibrain-side M6 work
is:

1. **Graph-feed endpoint.** A real-time or polling endpoint exposing
   *what changed* — which currently maps to the subscriptions spec
   item (§3.2/5). If subscriptions don't ship in M4, M6 will invent a
   polling version of it.
2. **Snapshot / rehydrate API.** `assemble_context` already exists; M6
   likely wants a "small initial graph render" shape — `Brain::snapshot(space)`
   returning entities + first-hop neighbors as JSON.
3. **`oxibrain-client` crate growth.** Currently 7 tool methods (verify
   with `grep "pub.*async fn" crates/oxibrain-client/src/`). M6 will add
   `subscribe`, `traverse`, `remember`, `retract`, `review`. The crate
   must stay thin — no `rusqlite`.

---

## 6. Recommend path for the next session

Given the time already invested in §12.2 patterns, I recommend the next
session picks up **§3.2.1 + §3.2.2** (`traverse`, `timeline` MCP tools +
resources) before M5:

- They fill the §12.2 spec table honestly.
- They don't require schema changes.
- They're additive — safe even if M5 reorders.
- Tests: read paths against the projection, identical to existing
  `Brain::traverse` tests.

If the user wants to skip them and go to M5, that's defensible too — none
of these block oxios consumption, which is M5's exit criterion. The decision
is whether to ship "DESIGN §12.2 implemented" before "ecosystem consuming".

---

## 7. Summary table — honest M4 status

| §12.2 spec item | Impl | Notes |
|---|---|---|
| 7 tools listed in `tools/list` | ✅ | search, recall, get_entity, ingest, declare, why, contradictions |
| 6 additional tools (traverse, timeline, remember, retract, merge, review) | ❌ | (5 missing entirely, 2 partially) |
| Resources (space://, entity://, episode://, graph://) | ❌ | |
| Long-running tasks (2026-07-28) | ❌ | ingest is sync |
| Subscriptions (2026-07-28) | ❌ | |
| Multi-round-trip / sampling (SEP-2322) | ✅ | shipped this session |
| HTTP transport (loopback) | ✅ | |
| Socket transport + token auth | ✅ | |
| Auth handshake as JSON-RPC method | ✅ | D-new-4 |
| Resources/subscribe protocol feature | ❌ | |
| Capabilities advertise (sampling, etc.) | ⚠️ | tools only |

**Honest score:** §12.2 surface is **~45%** of what DESIGN lists. The §12.2
narrative table that named "long-running tasks" and "subscriptions" as
M4's headline protocol features is **0% implemented**.

The Rust `Brain` facade (P6 — what oxios actually uses) is complete or
nearly so for M5. The standalone-v1 acceptance criteria
(`cargo install oxibrain && oxibrain init && oxibrain ingest && oxibrain ask`)
**all work today**. The MCP surface is the gap.

---

End of handoff. The §12.2 gaps above are the last M4 spec items before
oxios integration. M5 planning should treat them as either "do before M5"
(consumer-honesty) or "explicitly defer to M6 with ADR" (correctness
rhythm). No other M4 holes exist.
