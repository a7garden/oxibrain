# ADR-010: Vault watch is daemon-hosted (brain-owned connector)

**Date:** 2026-08-20 · **Status:** Accepted, implemented, verified
**Related:** ECOSYSTEM.md C3/C4, ARCHITECTURE.md §4.2 (v2.7 pull-connector
occurrence identity), §4.3/P8 (single writer), ADR-005 (lazy pull, unrelated
but same "where does it live" shape), `docs/superpowers/specs/2026-08-20-space-enumeration-design.md` §3 (amended by this ADR)

## Context

Closing the C4 loop — "when a note changes, the connector writes a new
episode" — needs something to re-run ingestion when vault files change. Two
documents assigned that responsibility to opposite owners:

- The oximemo integration spec (2026-08-18, §7/§8-D14) handed the watch
  connector **to oxibrain**: "커넥터는 브레인 소유" / "앱이 푸시하지 않음".
- The space-enumeration design (2026-08-20, §3 non-goals) recorded the
  opposite: "vault watching is **consumer-owned** … oximemo's vault connector
  watches".

Both readings are defensible from ECOSYSTEM.md C3 ("oxibrain … reads through a
connector" vs. "consumer owns its source of truth"). The observable result of
the contradiction: **no watcher existed in either repo**, and the production
vault had never been synced even once (0 vault-source episodes in the live
store at investigation time; the panel's recall could never return the user's
own notes).

## Decision

**The vault watcher is brain-owned and hosted inside the daemon process.**
Three pieces of evidence, in order of force:

1. **The P8 store lock decides it.** `serve` holds the exclusive advisory
   lock on the store for the daemon's lifetime (`lock.rs`, fail-fast
   `BrainError::Locked`). Any *external* recurring sync process would have to
   stop and restart the daemon on every pass — not automatable, not elegant.
   The only placement where recurring ingestion coexists with serving is the
   writer process itself.
2. **Connectors are brain modules.** C4 names "the connector" as the component
   that writes episodes; `oxibrain-connectors` already owns vault scanning
   (`.md` + `.html`) and now the debounced watcher (`watch::spawn_quiet`).
   Per-consumer watchers would duplicate debounce/classify/occurrence logic
   once per app for zero benefit.
3. **Registration already lives in the store.** `oxibrain sync` registers the
   vault as a pull source (`sources`: `kind = "document_revision"`,
   `mode = "pull"`, `name = canonical path`). The daemon enumerates exactly
   that set at startup — zero new configuration surface, and registration
   survives daemon restarts by construction.

## Shape (as implemented)

- `oxibrain::vault::{sync_vault, pull_sources}` — the scan → classify →
  ingest orchestration, moved out of the CLI so CLI, RPC, and watcher share
  one implementation (P6).
- `BrainServer::start_source_watchers` — called by every `serve_*`
  entrypoint; adopts each registered pull source into a debounced watcher
  (2 s quiet period; the C4 minimum-diff half is content-hash
  classification, which makes unchanged re-scans no-ops).
- `sync/run` native RPC (not a sixteenth MCP tool): registers a directory,
  runs one pass, adopts the watcher immediately. Scoped sessions need
  `trusted_ingest` + target-space membership.
- `oxibrain sync <dir>` — embedded pass when the store is free; on
  `BrainError::Locked` it attaches to the daemon over the default socket and
  runs the same pass via `sync/run`. The command works in both states.
- `Brain` is now `Clone` (cheap handle: Arc'd store actor + caches) so
  watcher threads share the serving brain.

## Consequences

- `oxibrain sync` is the single registration surface; the daemon is the
  single ingestion host. `oximemo` does nothing — its vault flows into the
  brain the moment the brain is pointed at the directory.
- The space-enumeration design's "consumer-owned watch" note is **superseded**
  by this ADR (amended in place with a pointer).
- Watcher keep-alives live in the serving listener's `BrainServer`; on the
  auth topology a watcher adopted by a session dies with that session's
  server handle and is re-adopted from the store at the next daemon start —
  safe under duplication because sync passes are idempotent.

## Verification (this change)

- `oxibrain-connectors`: watcher unit test — one tick per burst, coalescing.
- `oxibrain`: `pull_sources` round-trip after a `sync_vault` registration.
- `oxibrain-mcp`: `sync/run` round-trip + idempotence; scoped session without
  `trusted_ingest` → `UNAUTHORIZED`; live-FS watcher test (write → settle →
  episode count grows); client socket round-trip of `sync_run`.
- Gates: `cargo fmt --all -- --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test --workspace` — all green.
