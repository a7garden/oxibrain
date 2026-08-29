# Oxi Unified Home Layout

**Status:** Approved design
**Date:** 2026-08-29
**Scope:** oxibrain, oxios, oxicode, and oximemo local state

## Goal

Give the Oxi product family one discoverable home at `~/.oxi` while keeping
shared data and app-private state strictly separated by ownership.

## Canonical layout

```text
~/.oxi/
├── foundation/v1/                 # shared, non-secret contract
├── brain/                         # oxibrain-owned durable data
│   ├── brain.db
│   ├── documents.db
│   ├── documents.toml
│   ├── models/
│   └── runtime/
├── spaces/                        # space identity and user file containers
│   └── <space>/
│       ├── space.toml             # optional, non-secret metadata
│       └── vault/                 # user files, assets, trash, and git
├── oxios/                         # oxios-private state
├── oxicode/                       # oxicode-private state
└── oximemo/                       # oximemo-private state and derived index
```

There is no shared top-level configuration file and no top-level default space.
The directory layout is the installation registry. Each app keeps its own
schema/version markers and active-space preference. `OXI_HOME` overrides the
root for tests, portable installations, and deployment; its default is the
user's `~/.oxi`.

## Ownership and invariants

1. `brain/` is written only by oxibrain. It may read configured document roots
   under `spaces/*/vault` but never writes user files.
2. `spaces/*/vault` is the only multi-app write area. oximemo owns space/vault
   creation and migration; oxios and oximemo use the shared frontmatter and
   atomic-write contract.
3. `oxios/`, `oxicode/`, and `oximemo/` are private subtrees. No app writes
   another app's subtree.
4. Project-local `.oxicode/` remains project state and is not moved into the
   global home.
5. Secrets remain in OS Keychain where available or in app-private files with
   restrictive permissions. `foundation/v1` never contains secrets.
6. The active space is explicit at brain operation boundaries. UI “last space”
   state is app-private and does not become a global default.

## Resolution and compatibility

Each app resolves its own paths using this order:

1. explicit app-specific override (`OXIOS_HOME`, `OXICODE_HOME`,
   `OXIMEMO_HOME`, or an explicit vault/brain argument);
2. `OXI_HOME/<subtree>`;
3. default `~/.oxi/<subtree>`;
4. a legacy location, read-only, during the compatibility window.

An explicit override is never silently merged with a discovered legacy path.
If both old and new locations exist with different content, migration stops and
reports both paths.

## Migration

Migration is app-owned and resumable. It performs preflight checks, records a
journal in the destination subtree, uses rename when possible (copy + fsync +
verification across filesystems), and leaves the source as a recoverable backup.

- oxibrain moves `~/.oxi/models` to `~/.oxi/brain/models` and rewrites only its
  own document-root configuration/cache.
- oximemo moves `~/.oxi/vault/<space>` to
  `~/.oxi/spaces/<space>/vault`, or a legacy flat vault to
  `~/.oxi/spaces/personal/vault`, preserving `.git` and all files.
- oxios moves `~/.oxios` to `~/.oxi/oxios`.
- oxicode moves `~/.oxicode` to `~/.oxi/oxicode`; project-local `.oxicode` is
  untouched.
- oximemo registers the post-migration document root through the oxibrain
  client API. It never edits `brain/documents.toml` directly.

Legacy fallback is read-only for one compatibility release. Automatic deletion
of old paths is prohibited.

## Testing and release gates

Every resolver has hermetic `OXI_HOME` tests. Every migration has tests for
success, restart after each journal boundary, conflicting old/new paths, and
preservation of content and Git history. Cross-app tests verify shared-space
visibility and app-private ownership. Independent operation tests verify
oximemo and oxios continue to work when no brain process is available.

Release proceeds in two versions: compatibility release (dual-read and
previewable migration), then cutover release (legacy fallback removed after
observed successful migrations). The cutover never deletes user data.
