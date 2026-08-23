# ADR-011: Vault git history stays consumer-owned; the brain exposes a read-only occurrence query

**Date:** 2026-08-23 · **Status:** Accepted, implemented
**Related:** `ECOSYSTEM.md` C1/C3/C5, `ARCHITECTURE.md` §4.2 (pull-connector occurrence identity), ADR-010 (daemon-hosted vault watch), `CONSUMPTION_CONTRACT.md` 1.3

## Context

The vault (`~/.oxi/vault`) needed version control. Two candidate owners existed:

1. **oxibrain** — "knowledge and memory management" reads like version
   history's natural home, and the ledger already records every revision:
   `episodes.content` holds the full text per occurrence, chained by
   `occurrence_id = H(source_id, locator, predecessor, content_hash)` (§4.2.1).
2. **The authoring apps** (oxios first, then oximemo) — a local git layer
   (`GitLayer`, gix-based) shipped in oxios in May 2026 and has run in
   production since.

Putting git into oxibrain would be structurally tidy — one history store —
but breaks two non-negotiable ecosystem contracts:

- **C1 (additive, never load-bearing):** a user must recover a bad edit with
  the daemon stopped or uninstalled. File-restore over the socket makes the
  vault's undo path depend on an optional daemon.
- **C3 (never writes into a vault):** committing IS writing into the vault.
  The daemon stays read-only by design.

Two further mechanical gaps: the pull connector debounces (C4's
version-spam guard), so the newest seconds before a mistake may not be in
the ledger yet, while a synchronous local commit has no such gap; and the
ledger lives in `~/.oxi/brain/`'s SQLite — a different failure/backup domain
from the files it would be the sole history of.

Historical note: `GitLayer` predates the oxibrain extraction (added
2026-05-09; the RFC-047 memory migration landed 2026-08-15). It was never
part of the memory subsystem — the knowledge/memory split (RFC-003) kept it
on the file side from the start. Its placement is not extraction residue.

## Decision

**Two layers, independently owned, deliberately uncoordinated.**

1. **Mechanical safety net — shared crate `oxi-vault-git`** (crates.io,
   maintained in the oximemo repo beside `oxi-frontmatter`). Both oxios and
   oximemo auto-commit into the same vault repo. Ownership is claimed by a
   shared marker `.oxi-vault-git`; the historical `.oxios-git` marker is
   permanently recognized as legacy proof so existing installations never
   regress to "foreign repo" mode. Commits are synchronous, local, and work
   with the brain absent.
2. **Semantic history — the brain's occurrence chain**, exposed read-only by
   `Brain::episodes_for_locator` / native RPC `episodes/for_locator` /
   `BrainClient::episodes_for_locator` (Consumption Contract 1.3). No new
   storage: it queries `episodes` by `(source_id, source_ref=locator)` and
   returns the existing full-content chain, oldest first.

The layers do not sync, reference, or gate each other. Git commits and
brain occurrences are two independent observations of the same edit stream;
each keeps its own granularity (debounce windows differ) and neither is a
source of truth for the other.

## Shape (as implemented)

- `oxi-vault-git@0.1.0` — `GitLayer` extracted verbatim from
  `oxios-kernel::git_layer`; oxios consumes it via a `pub use` module swap
  (zero call-site churn).
- oximemo: `[git]` config (`auto_commit` default on, `adopt_foreign_repo`
  default off), watcher-fed consumer thread (non-blocking channel + burst
  coalescing; the ≤16 ms capture budget never touches gix), storage-pane
  toggle.
- oxibrain: store `episodes_for_locator` (one query, decision-free — P9),
  facade method compat-pinned, RPC read-gated like `resources/read`.
- oximemo `MemoDetail` renders the chain in a closable `HistoryPanel` that
  hides itself when the daemon is down (C1-shaped UI).

## Consequences

- Restoring an old version of a file is always possible offline via git;
  asking "how did my thinking about this note evolve" is always the brain's
  job. Neither feature is a substitute for the other.
- `ECOSYSTEM.md` §C5's vault tree comment now reads: shared git history
  (`oxi-vault-git`, oximemo + oxios), not `git history (oxios)`.
- If the ecosystem later wants single-layer history, the constraint to lift
  is C1/C3 themselves — this ADR should be superseded explicitly, never
  eroded silently.

## Verification

- `oxi-vault-git`: 41 tests (incl. `.oxios-git` legacy-marker recognition).
- oximemo desktop: behavioral test drives the real consumer — create →
  update commit, delete → removal commit (asserted on the full repo log;
  `log_for_file` only lists commits where the path exists in the tree).
- oxibrain: store test (A→B→A yields three chained episodes), server RPC
  test (chain length + scoped-session rejection without `read`), client
  socket round-trip.
- oxios: `vault_watcher_survives_runtime_spawn_callbacks` (real watcher +
  real auto-commit through the swapped crate) green.
