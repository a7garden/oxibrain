# Oxi Unified Home Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move the Oxi product family to one `OXI_HOME` root with top-level `brain`, `spaces`, and app-private subtrees while preserving existing data.

**Architecture:** The shared home contains only the brain data plane, space/vault containers, foundation contract, and isolated app subtrees. Each repository owns its resolver and migration for its own data; oximemo owns vault relocation and calls an idempotent oxibrain root-registration API.

**Tech Stack:** Rust 2024, Tokio, SQLite/rusqlite in oxibrain, Tauri/React in oximemo, TOML/JSON configuration, atomic rename/copy migration.

**Spec:** `docs/superpowers/specs/2026-08-29-oxi-home-layout-design.md`

## Global Constraints

- Default unified root is `~/.oxi`; `OXI_HOME` is the hermetic override.
- No shared top-level config file and no global default space.
- `brain/` is oxibrain-owned; `spaces/<space>/vault` is the only shared write area.
- Apps never write another app's subtree or edit `brain/documents.toml` directly.
- Legacy locations are read-only fallback during one compatibility release; no automatic deletion.
- Existing user changes in all repositories must be preserved.

### Task 1: oxibrain unified-root resolver and root-registration API

**Files:**
- Create: `crates/oxibrain/src/paths.rs`
- Modify: `crates/oxibrain-cli/src/main.rs`
- Modify: `crates/oxibrain/src/lib.rs`
- Modify: `crates/oxibrain-connectors/src/documents_config.rs`
- Test: `crates/oxibrain/src/paths.rs`
- Test: `crates/oxibrain-connectors/tests/documents_config.rs`

**Interfaces:**
- Produce `pub fn oxi_home() -> PathBuf` and `pub fn brain_dir() -> PathBuf`.
- Produce an idempotent `Brain` operation to register a `(space, alias, path)` document root.
- Preserve `--dir` as the highest-priority explicit brain override.

- [ ] Write resolver tests first: `OXI_HOME=/tmp/oxi-test` yields `/tmp/oxi-test/brain`; unset `OXI_HOME` with `HOME=/tmp/home` yields `/tmp/home/.oxi/brain`.
- [ ] Run `cargo test -p oxibrain paths` and confirm the new tests fail because the resolver does not exist.
- [ ] Implement the resolver and route the CLI default directory through it.
- [ ] Run the focused tests and then `cargo test -p oxibrain`.
- [ ] Add the idempotent root-registration decision/store path; test duplicate registration and replacement of the same `(space, alias)`.
- [ ] Run connector and facade tests; ensure no external app edits `documents.toml`.

### Task 2: oxibrain model migration and compatibility behavior

**Files:**
- Modify: `crates/oxibrain/src/models.rs`
- Modify: `crates/oxibrain-cli/src/cmd/model.rs`
- Create: `crates/oxibrain/src/migrate.rs`
- Test: `crates/oxibrain/src/migrate.rs`

**Interfaces:**
- Produce a resumable migration from `<oxi-home>/models` to `<oxi-home>/brain/models`.
- Preserve an existing explicit model directory and stop on conflicting old/new paths.

- [ ] Add failing tests for successful move, conflict detection, and restart after a journal is written.
- [ ] Run the focused migration tests and confirm expected failures.
- [ ] Implement preflight, journal, rename/copy verification, and read-only legacy fallback.
- [ ] Run focused tests, `cargo test -p oxibrain`, and `cargo clippy -p oxibrain --all-targets --all-features -- -D warnings`.

### Task 3: oximemo space/vault layout and migration

**Files:**
- Modify: `/Volumes/MERCURY/PROJECTS/oximemo/crates/oximemo-core/src/paths.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oximemo/crates/oximemo-core/src/spaces.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oximemo/apps/desktop/src-tauri/src/lib.rs`
- Create: `/Volumes/MERCURY/PROJECTS/oximemo/crates/oximemo-core/src/oxi_home.rs`
- Test: existing oximemo core path/space tests plus migration tests

**Interfaces:**
- Produce `oxi_home()`, `space_vault(home, name)`, and an idempotent vault migration entry point.
- Keep explicit `--vault`/`OXIMEMO_VAULT` as overrides; default to `OXI_HOME/spaces/<name>/vault`.

- [ ] Add failing tests for new default paths, flat-vault migration, already-per-space migration, conflict abort, and Git marker/history preservation.
- [ ] Run `cargo test -p oximemo-core paths spaces` and confirm expected failures.
- [ ] Implement path resolution and migration without deleting source data.
- [ ] Update desktop boot, CLI, settings, and watcher construction to use the resolver.
- [ ] Run focused tests, `cargo test -p oximemo-core`, and desktop Rust tests.

### Task 4: oxios private home and active-space resolution

**Files:**
- Modify: `/Volumes/MERCURY/PROJECTS/oxios/crates/oxios-kernel/src/config.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oxios/crates/oxios-kernel/src/brain/config.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oxios/crates/oxios-kernel/src/credential.rs`
- Create: `/Volumes/MERCURY/PROJECTS/oxios/crates/oxios-kernel/src/oxi_home.rs`
- Test: oxios kernel config/credential tests

**Interfaces:**
- Produce `oxi_home()` and `oxios_home()` with explicit `OXIOS_HOME` override.
- Resolve default knowledge root as `OXI_HOME/spaces/<space>/vault` while retaining explicit custom roots.

- [ ] Add failing tests for override precedence, new default workspace/config/auth paths, and legacy read-only discovery.
- [ ] Run the focused tests and confirm failures.
- [ ] Implement resolver, migration journal, and ownership-safe credential paths.
- [ ] Run kernel tests and web configuration tests.

### Task 5: oxicode global home and project-local boundary

**Files:**
- Modify: `/Volumes/MERCURY/PROJECTS/oxicode/oxicode-ai/src/product_env.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oxicode/oxicode-sdk/src/ports/fs/path.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oxicode/oxicode-sdk/src/ports/fs/catalog.rs`
- Modify: `/Volumes/MERCURY/PROJECTS/oxicode/oxicode-cli/src/setup_wizard.rs`
- Create: `/Volumes/MERCURY/PROJECTS/oxicode/oxicode-ai/src/oxi_home.rs`
- Test: product environment and SDK filesystem tests

**Interfaces:**
- Produce `oxi_home()` and `oxicode_home()` with `OXICODE_HOME` explicit override.
- Keep project `.oxicode/` discovery unchanged.

- [ ] Add failing tests for new global paths, explicit override, and project-local discovery.
- [ ] Run focused tests and confirm failures.
- [ ] Implement resolver and migration-safe auth/catalog/session paths.
- [ ] Run SDK, AI, CLI, and full workspace tests.

### Task 6: compatibility release checks and documentation

**Files:**
- Modify: `/Volumes/MERCURY/PROJECTS/oxibrain/doc/ECOSYSTEM.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxibrain/doc/ARCHITECTURE.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxibrain/README.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxibrain/CHANGELOG.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxios/README.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxios/CHANGELOG.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxicode/README.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oxicode/CHANGELOG.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oximemo/README.md`
- Modify: `/Volumes/MERCURY/PROJECTS/oximemo/CHANGELOG.md`
- Create: cross-project fixture documentation under `tests/fixtures/oxi-home/`

- [ ] Add fixture cases for clean install, legacy install, partial migration, and conflicting locations.
- [ ] Run the complete Rust suites, frontend suites, formatting, clippy, and standalone dependency checks.
- [ ] Run a cloned-home smoke migration without touching the real user home.
- [ ] Record the compatibility release checklist and rollback procedure.
