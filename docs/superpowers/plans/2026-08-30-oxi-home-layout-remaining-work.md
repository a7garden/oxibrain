# Oxi Unified Home Layout — Remaining Work

**Date:** 2026-08-30  
**Parent design:** [`2026-08-29-oxi-home-layout-design.md`](../specs/2026-08-29-oxi-home-layout-design.md)  
**Implementation plan:** [`2026-08-29-oxi-home-layout.md`](./2026-08-29-oxi-home-layout.md)

**Status:** Implemented 2026-08-30. oxibrain v0.12.0, oximemo v0.13.0,
oxicode v0.79.0 released (compatibility window open; legacy fallbacks
removed only in a later cutover release). oxios work landed on
`feat/brain-chat-binding` untagged — its compatibility release rides the
branch's merge to main.

## Target contract

```text
~/.oxi/
├── foundation/v1/                 # shared, non-secret contract
├── brain/                         # oxibrain-owned data and models
├── spaces/<space>/vault/          # shared user files
├── oxios/                         # oxios-private state
├── oxicode/                       # oxicode-private state
└── oximemo/                       # oximemo-private state and indexes
```

The root may be overridden with `OXI_HOME`. There is no shared root config and
there is no implicit global default space. Project-local `.oxicode/` remains
project state. Legacy paths are read-only compatibility sources until the
cutover release.

## Current implementation status

### Already landed in the working trees

- Canonical `OXI_HOME`/`brain` resolver exists in oxibrain.
- oxibrain CLI defaults to `~/.oxi/brain`.
- oxibrain model lookup prefers `~/.oxi/brain/models` and can read the old
  `~/.oxi/models` location during compatibility.
- oxibrain space provisioning and removal tests now target
  `~/.oxi/spaces/<space>/vault`.
- oximemo resolves spaces below `~/.oxi/spaces` and keeps derived state below
  `~/.oxi/oximemo`.
- oximemo has existing flat-vault and application-support migration logic,
  updated to the new destination layout.
- oxios has an `oxi_home` resolver, nested default config/workspace/log/pid
  paths, and a `~/.oxi/spaces/personal/vault` knowledge-root fallback.
- oxicode’s product-home resolver now defaults to `~/.oxi/oxicode`; project
  `.oxicode/` discovery remains separate.
- Focused resolver/migration tests have been run for the changed paths.

These changes are not yet a release. The remaining work below is required.

## P0 — correctness and ownership blockers

### 1. Finish oxibrain’s registration boundary

oxibrain must expose an idempotent in-process operation for registering a
document root `(space, alias, path, include, exclude, limits)`. The operation
must be owned by oxibrain and persist through its normal write path. oximemo
must call this operation through the client/stdio boundary after a space is
created or migrated.

- [ ] Add the typed facade/API operation and duplicate/replacement semantics.
- [ ] Add client support for the operation.
- [ ] Remove oximemo’s direct writes to `brain/documents.toml`.
- [ ] Add a cross-process test proving that a migrated oximemo vault is
  registered without oximemo opening or writing the brain database itself.
- [ ] Preserve offline behavior: oximemo remains usable when no brain process
  is installed; registration then becomes a recorded/deferred action.

### 2. Implement resumable migrations

Each app needs a journaled migration with preflight, conflict detection,
atomic rename when possible, copy-plus-verification across filesystems, and a
recoverable source. A path fallback alone is not sufficient for a user with
several gigabytes of existing state.

- [ ] oxibrain: migrate `<OXI_HOME>/models` → `<OXI_HOME>/brain/models`.
- [ ] oxios: migrate legacy `~/.oxios` → `<OXI_HOME>/oxios`.
- [ ] oxicode: migrate legacy `~/.oxicode` → `<OXI_HOME>/oxicode`.
- [ ] oximemo: finish migration coverage for old app-support vaults and old
  flat `~/.oxi/vault` layouts, including conflict and restart states.
- [ ] Write a journal before the first filesystem mutation and make every
  journal step retryable.
- [ ] Refuse to merge conflicting old/new locations silently; report both
  paths and leave both untouched.
- [ ] Never delete the source automatically during the compatibility release.

### 3. Eliminate remaining global legacy path reads

Search each repository for runtime uses of `~/.oxios`, `~/.oxicode`,
`~/.oxi/vault`, and `~/.oxi/models`. Keep only intentional migration,
read-only compatibility, fixture, or historical-document references.

- [ ] oxios: audit web distribution, kernel assets, agent-log, foundation,
  permission, backup, and launcher paths.
- [ ] oxicode: audit session, extension, package, hook-approval, slash-command,
  reset, and diagnostics paths.
- [ ] oximemo: audit `HOME`-only resolution so `OXI_HOME` works consistently in
  space selection, doctor, watcher, and migration code.
- [ ] oxibrain: update all production defaults and provisioning comments to
  `spaces/<space>/vault` and `brain/models`.

## P1 — compatibility and safety

### 4. Define migration commands and operator UX

- [ ] Add a dry-run command for each app showing source, destination, bytes,
  conflicts, and required user action.
- [ ] Add an explicit `migrate`/`doctor` action that can resume a journal.
- [ ] Print the active `OXI_HOME`, space, owned subtree, and legacy state in
  diagnostics output.
- [ ] Ensure credentials remain app-private and use restrictive permissions;
  do not copy secrets into `foundation/v1`.
- [ ] Document rollback: stop the app, preserve the destination journal and
  source backup, then restore only after verification.

### 5. Add shared fixtures and cross-app tests

Create a fixture matrix under `tests/fixtures/oxi-home/` (or the equivalent
repository-local fixture roots):

- [ ] clean install with one space;
- [ ] legacy install for every app;
- [ ] partially completed migration at every journal boundary;
- [ ] old and new locations both populated with identical content;
- [ ] old and new locations populated with conflicting content;
- [ ] multiple spaces with one shared vault visible to oxios and oximemo;
- [ ] no brain process available;
- [ ] `OXI_HOME` pointing at a temporary directory.

Every fixture must assert that files, permissions, `.git` history, and indexes
are preserved as applicable.

## P2 — documentation and release preparation

### 6. Make repository documentation match the contract

- [ ] Update oxibrain `ARCHITECTURE.md`, `ECOSYSTEM.md`, README, and changelog.
- [ ] Update oxios README/changelog and default-config examples.
- [ ] Update oxicode README/changelog and all user-facing path diagnostics.
- [ ] Update oximemo README/DESIGN/changelog and migration instructions.
- [ ] Mark old paths as legacy in code comments; remove contradictory claims
  that a shared `~/.oxi/config.toml` or a global default space exists.
- [ ] Add an ADR for the “top-level spaces + app-private subtrees” ownership
  decision and the no-shared-config rule.

### 7. Version and release the compatibility window

- [ ] Bump the relevant architecture/schema/config compatibility versions.
- [ ] Add release notes describing the new layout, dry-run migration, source
  retention, and `OXI_HOME` override.
- [ ] Build signed/reproducible binaries or app bundles for oxibrain, oxios,
  oxicode, and oximemo.
- [ ] Run installation smoke tests in a cloned temporary home; never migrate
  `/Users/won/.oxi`, `/Users/won/.oxios`, or `/Users/won/.oxicode` as part of
  development verification.
- [ ] Publish the compatibility release first. Remove legacy fallback only in
  a later cutover release after migration telemetry/feedback is reviewed.

## Required verification before claiming completion

Run from each repository (with temporary `OXI_HOME`/`HOME` where relevant):

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --all-targets --all-features -- -D warnings
```

Additional gates:

```bash
# oxibrain standalone guarantee
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1

# oxibrain deterministic projection and document tests
cargo test -p oxibrain -- truth_reprojection
cargo test -p oxibrain-connectors

# oximemo migration and space tests
cargo test -p oximemo-core -- migrate_spaces
cargo test -p oximemo-core -- migrate_vault

# oxios/oxicode path and compatibility tests
cargo test -p oxios-kernel -- config
cargo test -p oxicode-catalog -- product_env
cargo test -p oxicode-cli -- settings
```

Completion requires green results, a clean migration smoke run in a cloned
home, and a release artifact for every product. Focused tests passing while a
migration journal, ownership boundary, or release build is missing does not
count as complete.
