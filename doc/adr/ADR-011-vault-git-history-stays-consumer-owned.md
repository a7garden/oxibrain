# ADR-011: Vault git history stays consumer-owned; gix-backed read-only document history (v2.11 amendment)


**Date:** 2026-08-23 · **Status:** Accepted, implemented; **amended 2026-08-27** by the
v2.11 two-plane, no-daemon cutover (`docs/superpowers/specs/2026-08-27-two-plane-documents-and-memory-design.md`).
**v2.11 §4.5 amendment:** the "semantic history" half of this ADR — the read-only
`episodes_for_locator` query and the `sync/run` occurrence chain that fed it — is
**retired**. The new history surface is gix-backed: `Brain::document_history` (and the
`document_history` native JSON-RPC method) walks the consumer-owned git tree for
`git:<format>:<oid>` revisions, oldest first, exactly like `oxi-vault-git::log_for_file`.
The mechanical write-side guarantee — writes stay with oximemo/oxios via
`oxi-vault-git`, oxibrain never initializes or commits a repo — is unchanged. The
consumer-owned and read-only invariants of this ADR therefore survive in full; only
the implementation of "read" changed.

**Related:** `ECOSYSTEM.md` C1/C3/C5, `ARCHITECTURE.md` §4.2.1 (documents plane), ADR-010 (now superseded), `CONSUMPTION_CONTRACT.md` 1.4

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
2. **Semantic history — the brain's read-only gix document query**, exposed by
   `Brain::document_history(space, alias, locator, limit)` / native RPC
   `document_history` / `BrainClient::document_history` (Consumption Contract
   1.4). It does **not** query `episodes`; it opens the vault repo read-only
   through `gix` and returns the tree walk ordered by commit time (mirrors
   `oxi-vault-git::log_for_file`, which is why both stay in lock-step).

The layers do not sync, reference, or gate each other. Git commits are an
independent observation of edits; the brain surfaces them without duplicating
them. There is no per-occurrence ledger row, no `occurrence_id` derivation,
no `sync/run` round-trip — the v2.11 cutover retired the pull connector and
the occurrence chain with it.

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
- oxibrain: `GitDocumentReader` integration test (HEAD snapshot, history
  oldest-first, dirty worktree ⇒ `blake3:` revision, clean tracked ⇒ `git:`
  revision, `is_ignored` honors repo excludes, `rename_hint` resolves
  unambiguous renames), facade `document_history` round-trip, native RPC
  `document_history` with read gating, client `BrainClient::document_history`
  pipe-delimited stdio test. No `episodes_for_locator` test remains.
- oxios: `vault_watcher_survives_runtime_spawn_callbacks` (real watcher +
  real auto-commit through the swapped crate) green.
