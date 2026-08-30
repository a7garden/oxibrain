# ADR-015 — Unified Oxi home: top-level spaces, app-private subtrees, no shared config

Status: accepted (compatibility release). Date: 2026-08-30.

## Context

Four apps — oxibrain, oxios, oxicode, oximemo — each kept their own state in
independent homes (`~/.oxi/brain`, `~/.oxios`, `~/.oxicode`, app-support
vaults, and a flat `~/.oxi/vault` that three of them touched). Two earlier
attempts at sharing drifted:

1. **The shared `~/.oxi/config.toml`.** §18 once specced it as the
   ecosystem-wide user config (`[vault].space`, `default_space`); it was
   implemented for `default_space` and then deleted at v2.13 (ADR-013:
   machine-local resolution makes the same call mean different things per
   machine, and silent defaults route agent writes into the wrong space).
   The remaining "shared" keys have the same defect: a shared config file is
   a hidden coupling point between independently released apps.
2. **The flat `~/.oxi/vault`.** Provisioned per-space subdirectories lived
   under one shared root with no owner: every app walked (and oximemo
   provisioned) the same tree, derived state wanted in, and "which app owns
   this directory?" had no answer.

Meanwhile users had real data in four legacy homes. A layout change without
a migration path would strand multi-gigabyte model/vault state.

## Decision

1. **One root, strict ownership.** `~/.oxi` (override `OXI_HOME`) is the
   single installation registry:

   - `foundation/v1/` — shared, non-secret Foundation contract.
   - `brain/` — oxibrain-owned data and models (its default `--dir`).
   - `spaces/<space>/vault/` — the **only** shared write area (user files,
     frontmatter contract, git). oximemo owns space provisioning; oxios and
     oximemo read it as a document root.
   - `oxios/`, `oxicode/`, `oximemo/` — app-private subtrees. No app writes
     another app's subtree, ever.

2. **No shared config file, no global default space.** Each app keeps its
   own schema/version markers and active-space preference in its private
   subtree. Cross-app identity comes from the directory layout, not from a
   negotiated file. This closes the "shared config" experiment for good:
   any future cross-app key must be a *declared* interface (like
   `foundation/v1/`), not ambient state.

3. **Registration boundary, not hand-edited config.** `documents.toml`
   stays oxibrain-owned and is now written only by oxibrain, atomically.
   Other apps call the idempotent `register_document_root` operation
   (facade method → native JSON-RPC method → `BrainClient` method; upsert
   keyed by alias with added / replaced / unchanged semantics). When no
   brain binary is installed, the caller records a pending registration and
   replays it later — offline remains a first-class state.

4. **Journaled, resumable, source-preserving migrations.** Legacy layouts
   (`~/.oxi/models`, the flat `~/.oxi/vault`, app-support vaults,
   `~/.oxios`, `~/.oxicode`) are read read-only during one compatibility
   release while per-app migrations consolidate them. Every migration:
   prefights (dry-run: source, destination, bytes, conflicts), writes a
   journal before the first mutation, copies with per-file verification
   (rename is deferred to the cutover release), refuses conflicting
   old/new pairs with both paths reported, and **never deletes the source**.

## Consequences

- Consumers change behavior once: boot-time vault registration goes through
  the client (oximemo), or is deferred; legacy reads fall back read-only.
- The compatibility window carries duplicated state (legacy + canonical);
  `doctor` surfaces both plus the journal so operators can verify and
  delete backups manually. The cutover release removes legacy fallbacks —
  never data.
- App-private subtrees make "which app owns this?" answerable by path,
  which is what makes the deny-gate policy (`.oxi`, `.oxicode` off-limits
  to agents) enforceable per subtree.
- Space provisioning stays with oximemo (it owns vault UX); oxibrain keeps
  provisioning only its per-space documents.toml roots.

## References

- Spec: `docs/superpowers/specs/2026-08-29-oxi-home-layout-design.md`
- Remaining work: `docs/superpowers/plans/2026-08-30-oxi-home-layout-remaining-work.md`
- ADR-013 (no default space / no machine-local resolution)
- ADR-011 (vault git history stays consumer-owned)
