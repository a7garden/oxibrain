# ADR-001: Defer Long-Running Tasks and Subscriptions to M6

> **Note (2026-08-13):** `DESIGN §n` references below use `DESIGN.md` v1.0 numbering.
> That file is now `doc/ARCHITECTURE.md` v2.0 and its sections were renumbered. This ADR is a
> historical record and is left as written.

- **Status:** Deferred
- **Date:** 2026-08-12
- **Supersedes:** none
- **Superseded by:** (none yet)

## Context

DESIGN §12.2 names two 2026-07-28 protocol features as M4 headline items:

1. **Long-running tasks** — `ingest` becomes a protocol task (`tasks/create`,
   `notifications/progress`, `tasks/cancel`) instead of a synchronous
   `tools/call`.
2. **Transport-neutral subscriptions** — push notifications for new
   contradictions, finished extractions, and merge candidates.

All other §12.2 spec items are now implemented: 14 MCP tools, 4 resources,
capabilities handshake, sampling, HTTP/socket transports, token auth.

## Decision

Defer both features to M6 (Product / Desktop UI), not M5 (oxios migration).

### Rationale

1. **Neither blocks M5.** oxios consumes the `Brain` Rust facade (P6), not MCP.
   The `Brain` API already covers everything oxios needs. M5's exit criterion —
   "oxios ships with zero memory code of its own" — is independent of these MCP
   protocol features.

2. **Both are substantial.** Long-running tasks need a task state store,
   background processing, progress streaming, and cancellation semantics
   (~2-3 days). Subscriptions need a pubsub layer and notification handlers
   (~1-2 days), and depend on tasks for the `extracted` notification.

3. **Polling fallback exists.** The `graph://` resource, `resources/list`,
   `contradictions` tool, and `review_merges` tool all provide polling-based
   access to the information that subscriptions would push. M6's desktop UI can
   poll until subscriptions ship.

4. **M6 is the natural home.** M6 (Desktop brain UI) is where a live,
   push-driven UX matters most. The contradiction inbox, merge review, and
   extraction status are M6 UI features. Shipping the protocol machinery
   alongside the UI that consumes it avoids building notification plumbing
   that nobody uses yet.

## Consequences

- The §12.2 scorecard is ~85%, not 100%. The 15% gap is these two features.
- `ingest` remains synchronous via MCP. For large corpus ingestion, use the CLI
  (`oxibrain ingest --watch`) or the job queue (`Brain::ingest` +
  `Brain::extract_pending`), not the MCP `ingest` tool.
- M6 will implement both features together: task lifecycle + subscription
  pubsub + the UI that consumes them.

## Revisit trigger

When M6 starts, or when a third-party MCP consumer explicitly requests
progress notifications for long-running ingest.
