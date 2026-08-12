# Handoff — M4 §12.2 Spec Gaps Closed (Tools + Resources)

> **Status:** All missing §12.2 MCP tools and resources shipped. The §12.2
> tools table and resources line are now fully implemented. Long-running tasks
> and subscriptions (2026-07-28 protocol features) are explicitly deferred —
> see ADR-001.
> **Branch:** `main`
> **Predecessor:** `2026-08-12-m4-spec-gaps-to-m5.md`
> **Tests:** 216 pass (was 202), 0 fail. Clippy clean. Fmt clean. Standalone verified.

---

## 1. What shipped this session

### 1.1 New MCP tools (7 added, 14 total)

| Tool | Caps | Status |
|---|---|---|
| `search` | Read | ✅ (existing) |
| `recall` | Read | ✅ (existing) |
| `get_entity` | Read | ✅ (existing) |
| **`traverse`** | Read | ✅ **new** — bounded subgraph from start entities |
| **`timeline`** | Read | ✅ **new** — belief intervals over a time range |
| `why` | Read | ✅ (existing) |
| `contradictions` | Read | ✅ (existing) |
| **`review_merges`** | Read | ✅ **new** — list merge records in a space |
| `ingest` | Ingest | ✅ (existing, sync) |
| **`remember`** | Write | ✅ **new** — one-shot ingest + sync extraction |
| `declare` | Write | ✅ (existing) |
| **`retract`** | Write | ✅ **new** — writes a denying assertion via Declaration::Retract |
| **`merge_entities`** | Write | ✅ **new** — merges two entities via Declaration::Merge |
| **`redact`** | Redact | ✅ **new** — destructive redaction with dry-run support |

### 1.2 Resources (entirely new)

| URI scheme | Handler | Status |
|---|---|---|
| `resources/list` | — | ✅ returns concrete `space://` + 4 URI templates |
| `resources/read` | — | ✅ dispatches by scheme |
| `space://{name}` | entity count, episode count, contradictions, recent entities | ✅ |
| `entity://{id}?space=` | entity beliefs (same as get_entity) | ✅ |
| `episode://{id}` | full episode record | ✅ |
| `graph://{entity}?depth=n&space=&direction=` | bounded subgraph (same as traverse) | ✅ |

### 1.3 Capabilities handshake

`initialize` now advertises `resources: { listChanged: false, subscribe: false, read: true }`
alongside the existing `tools` capability.

### 1.4 Store + facade additions

- `oxibrain_store::knowledge::list_entities(conn, space, limit)` — newest-first, excludes merged.
- `oxibrain_store::knowledge::list_merges(conn, space)` — merge records by space.
- `Brain::list_entities(space, limit)` / `Brain::list_merges(space)` — async facade wrappers.

### 1.5 Tests (14 new)

- `tools_list_advertises_full_surface` — all 14 tools present with schemas.
- `traverse_returns_subgraph` / `traverse_missing_start_is_invalid_params`
- `timeline_returns_entries`
- `review_merges_lists_records` (merge then review round-trip)
- `retract_denies_a_statement`
- `redact_dry_run_previews_closure`
- `remember_ingests_without_session`
- `scoped_read_denies_redact` (capability gate)
- `resources_list_returns_templates`
- `resources_read_space_returns_overview`
- `resources_read_entity_returns_beliefs`
- `resources_read_episode_returns_record`
- `resources_read_graph_returns_traversal`
- `resources_read_unknown_scheme_is_error`
- `initialize` test now checks `capabilities.resources`.

---

## 2. What remains deferred (ADR-001)

### 2.1 Long-running `ingest` (2026-07-28 protocol tasks)

Currently `ingest` is synchronous `tools/call`. The protocol-task variant would
return a task-id immediately, stream `notifications/progress`, and support
`tasks/cancel`. This requires:

- A task state store (or reuse `ingest_jobs`).
- Background processing with progress reporting.
- Cancellation semantics.

**Deferred because:** it is the largest single item (~2-3 days), the sync path
works for standalone use, and it does not block M5 (oxios uses the Rust `Brain`
facade, not MCP). See `doc/adr/ADR-001-defer-protocol-tasks-subscriptions.md`.

### 2.2 Subscriptions

Push notifications for new contradictions, finished extractions, and merge
candidates. Requires an in-memory pubsub or subscription table plus
`notifications/*` handlers.

**Deferred because:** it depends on long-running tasks for the `extracted`
notification, and the `graph://` resource + `resources/list` already provides a
polling fallback. M6 will likely need subscriptions for a live UI, at which
point both features ship together.

### 2.3 Interactive merge review UX

`review_merges` is read-only (lists records). The interactive accept/reject flow
is M6 territory (DESIGN §17: "merge review" is a desktop UI feature).

---

## 3. Updated §12.2 scorecard

| §12.2 spec item | Before | After |
|---|---|---|
| 7 tools (original) | ✅ | ✅ |
| 7 additional tools (traverse, timeline, remember, retract, merge, review, redact) | ❌ | ✅ all 7 |
| Resources (space://, entity://, episode://, graph://) | ❌ | ✅ |
| Long-running tasks (2026-07-28) | ❌ | ❌ deferred (ADR-001) |
| Subscriptions (2026-07-28) | ❌ | ❌ deferred (ADR-001) |
| Multi-round-trip / sampling | ✅ | ✅ |
| HTTP transport | ✅ | ✅ |
| Socket transport + token auth | ✅ | ✅ |
| Capabilities advertise (tools + resources) | ⚠️ tools only | ✅ |

**§12.2 surface is now ~85%** — all tools and resources are implemented. The
remaining 15% is the two 2026-07-28 protocol features (long-running tasks,
subscriptions), explicitly deferred with ADR-001.

---

## 4. Design notes

### 4.1 `retract` and `merge_entities` are thin wrappers

The `Declaration` enum already supports `Retract` and `Merge` variants
(`oxibrain_store::project::Declaration`). The MCP tools deserialize JSON args
into `EntityRef` / `DeclObject` via `serde_json::from_value`, build the
`Declaration`, and call `Brain::declare`. No new core logic was needed.

### 4.2 Resource URI space convention

- `space://{name}` — the path IS the space name.
- `entity://{id}?space=name` — space via query param (default: `personal`).
- `episode://{id}` — episodes have global IDs, no space needed (but `?space=` is accepted).
- `graph://{entity}?depth=n&space=name&direction=out|in|both` — traversal params as query.

### 4.3 `remember` vs `ingest`

`remember` is `ingest` with `extract=true` forced and `source_path="remember"`.
Both use the same `try_sample_extract` path: if a sampling session is available,
extract via the client's model; otherwise, skip extraction and note it in the
response.

---

## 5. Path to M5

The §12.2 gaps that block M5 are **none** — oxios consumes the `Brain` Rust
facade, not MCP. The `Brain` facade is stable and covers everything oxios needs:

- `ingest`, `query`, `assemble_context`, `declare`, `beliefs`, `why`, `redact`,
  `traverse`, `timeline`, `list_entities`, `list_merges`, `resolve_entity_id`.

M5 work (per DESIGN §17):
1. **Consumption contract 1.0** (§16.4) — pin public surface, tag stability tiers.
2. **Importer for `oxios-memory` stores** → `oxibrain` episodes.
3. **C1 fallback decision** (§16.1).
4. Update `oxios-kernel` to depend on `oxibrain::*`.

---

End of handoff. M4 §12.2 tools + resources are done. Long-running tasks and
subscriptions are the only remaining §12.2 items, explicitly deferred per
ADR-001. M5 can start.
