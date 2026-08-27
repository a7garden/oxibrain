# Space Lifecycle & Default Configuration — Design

> **Date:** 2026-08-27 · **Status:** approved for implementation (user directive:
> toml-first configuration, per-space vault dirs under `~/.oxi/`, carry to
> implementation-ready state)
> **Scope:** make spaces a first-class user-manageable unit — explicit
> create/remove, configurable default, per-space vault provisioning — without
> introducing any new concept above spaces.

## 1. Problem

Spaces exist and isolate correctly (§15.1), but they are an implementation
concept, not a managed one:

1. **The default space is hardcoded.** Every CLI verb defaults
   `--space` to `"personal"` (`crates/oxibrain-cli/src/cli.rs`, 15 occurrences);
   the MCP server falls back to `"personal"` for tools that omit `space`
   (`crates/oxibrain-mcp/src/server.rs:158`). A coding agent using a `dev`
   space must pass the flag on every call; one omission silently reads and
   **writes** the personal space.
2. **Spaces cannot be managed.** `ensure_space` implicitly creates a space on
   any verb (`ask --space persnal` creates a `persnal` row on a *read* path —
   the same shadow-row leak §16.2 fixed for scoped MCP sessions). There is no
   command to add a space deliberately, none to remove one, and no way to see
   which space is the working default.
3. **No per-space document roots are provisioned.** `documents.toml` roots map
   a directory to a space, but creating a space does not give it a place in the
   `~/.oxi/` layout; the operator hand-writes `[[root]]` entries.
4. **`~/.oxi/config.toml` is specced but unimplemented.** §18 documents it as
   "shared: which brain, which space, provider settings"; no code reads it.

User decision (this session): **a space *is* a profile.** No profile layer, no
context auto-detection, no physical brain separation. What is missing is
lifecycle management and a real default.

## 2. Goals

- `oxibrain space add|remove|default` + upgraded `oxibrain spaces` listing.
- Default space resolved from `~/.oxi/config.toml` (toml, not env), used
  identically by CLI verbs and MCP tools that omit `space`.
- Every space provisioned with its own vault dir `~/.oxi/vault/<space>/` and a
  matching `documents.toml` root (default brain dir only).
- Removal is P5-compliant: destruction only through audited redaction
  (`--purge`); empty spaces remove directly; user files in vault dirs are never
  deleted by oxibrain.
- Implicit space creation on ordinary verbs is abolished; creation happens in
  `init`, `space add`, and archive `import` only.

## 3. Non-goals (with reasons)

- **`OXIBRAIN_SPACE` env var** — user preference: toml over env. `--dir` /
  `OXIBRAIN_DIR` keep their existing behavior (pre-existing surface).
- **`dir` / provider keys in config.toml** — reserved by §18, not parsed yet;
  adding them is a separate change with its own failure modes. YAGNI.
- **`space rename`** — the space id is `blake3(name)`
  (`oxibrain-store/src/ledger.rs:34`), so a rename *is* a new space. Removing
  and re-adding is the honest operation.
- **Cross-space or multi-space query** (`ask --global`) — orthogonal retrieval
  work; unchanged by this spec.
- **Space admin over MCP (native RPC `spaces/create`/`spaces/delete`)** — CLI
  first. The operations console can grow native RPC when it needs it.
- **Migration of an existing flat `~/.oxi/vault` root** — existing
  `documents.toml` is never rewritten beyond additive provisioning (below);
  reorganizing a live vault is a user decision.

## 4. Design

### 4.1 User config — `~/.oxi/config.toml`

First implementation of the §18 file. One parsed key:

```toml
# ~/.oxi/config.toml
default_space = "personal"
```

- Missing file ⇒ built-in default `"personal"`. No file is created unless a
  command writes it (`init`, `space default`).
- Malformed file ⇒ hard error naming path and parse message, on every command.
  A config typo must not silently change which space data lands in.
- Unknown keys are ignored (forward compatibility with §18's reserved keys).
- Lives in `oxibrain` (facade crate) as `crates/oxibrain/src/config.rs`:
  `UserConfig { default_space: String }`, `load(home: &Path)` (strict parse),
  `set_default_space(home, name)` writing via `toml_edit` so comments and
  unknown keys survive. CLI and MCP-server already depend on the facade.

**Default-space resolution (one rule, everywhere):**

```
--space <flag>   >   ~/.oxi/config.toml default_space   >   "personal"
```

### 4.2 CLI surface

```
oxibrain spaces                       # list: name, episodes, entities, documents, '*' marks default
oxibrain space add <name>             # create + provision (idempotent)
oxibrain space remove <name> [--purge]
oxibrain space default [<name>]       # no arg: print current; with arg: set (validates + writes config.toml)
```

- `spaces` (plural, existing) stays the list verb; `space` (singular) gains the
  management subcommands. No breaking change to existing invocations.
- The listing joins both stores: episode/entity counts from `brain.db`,
  document count (rows in `documents`) from `documents.db`, default marker
  from resolved config.
- `space default <name>` validates the space exists **in the resolved brain**
  before writing; refuses to write a config pointing at a space that is not
  there.

**Name validation** (add / init / default): trimmed; length 1–64 chars;
allowed characters: Unicode letters, digits, `-`, `_`. No whitespace, no `/`
or `\`, no `:`, no control characters. Rationale: the name becomes a directory
name and a config token; letters/digits are kept open for non-Latin names
(consistent with C3 — this is an identifier rule, not content processing).

### 4.3 Per-space vault provisioning

When the resolved brain dir **is the default** `~/.oxi/brain` (same guard as
init today: an explicit `--dir` is a deliberate store and the foundation
layout is not touched), `space add <name>` and `init` provision:

1. `mkdir -p ~/.oxi/vault/<name>/`
2. Append to `<brain-dir>/documents.toml`, if no root with this alias exists:
   `[[root]]` with `alias = <name>`, `path = "~/.oxi/vault/<name>"`,
   `space = <name>`, default include/exclude/max_file_bytes.
   Alias collision with a different path/space ⇒ error.
3. **Parent fixup:** any existing root whose canonicalized path equals
   `~/.oxi/vault` (a legacy flat root) gains `exclude += ["<name>/**"]` if
   absent — otherwise the flat root would double-index the new space's
   documents into `personal`.

Non-default brain dir: create the space row only, print a hint that roots are
operator-managed for deliberate stores.

`init [--space S]` changes: the `seed_target` flat-vault seeding
(`crates/oxibrain-cli/src/cmd/init.rs:58`) is replaced by per-space
provisioning of `S`; `~/.oxi/config.toml` is written with
`default_space = S` **only when the file does not exist** (never overwrite).
Existing documents.toml/config are never clobbered; provisioning steps are
idempotent.

### 4.4 Auto-create abolition

`ensure_space` remains in exactly three callers: `init`, `space add`,
`import` (a restore path — export archives include the `spaces` table,
`crates/oxibrain-store/src/export.rs:15`). Every other CLI verb and every MCP
tool resolves the space with the read-only `lookup_space`:

- unknown space ⇒ error: `space '<name>' not found — create it with:
  oxibrain space add <name>` (non-zero exit; MCP tool error with the same
  text).
- This closes the CLI-side shadow-row leak and makes a typo in front of an
  append-only ledger a hard stop instead of a permanent misfiled episode
  (fixing a misfiled episode requires redaction).

Mechanically: drop `default_value = "personal"` from the clap args; the verb
receives `Option<String>`; a `resolve_space(flag: Option<&str>, home)` helper
applies §4.1's order and returns the name; each command's first store call
becomes `lookup_space` with the error above.

### 4.5 Removal and purge (P5)

`space remove <name>`:

- unknown space ⇒ error.
- `name == resolved default_space` ⇒ error, "change the default first:
  `oxibrain space default <other>`".
- **Empty check** (all must hold, else refuse with the enumerated reasons and
  a `--purge` hint):
  1. `episodes` count for the space is 0 (entities/beliefs/extractions derive
     from episodes; 0 episodes ⇒ nothing else references it in `brain.db`),
  2. `documents.db` chunk count for the space is 0,
  3. no `documents.toml` root references the space **except its own
     provisioning scaffold** — the root with `alias == name` and
     `path == ~/.oxi/vault/<name>`. A freshly added, never-used space is
     empty; its scaffold is not content. Any other root (user-authored
     path/alias) still refuses.
- Empty removal: remove the scaffold `[[root]]` from `documents.toml` first
  (abort on edit failure — nothing destroyed), drop the `spaces` row, sweep
  any space-scoped `documents.db` metadata rows (cache; safe), and remove
  `~/.oxi/vault/<name>/` only if it is empty (never delete files).

`space remove <name> --purge` — destruction is redaction, audited:

1. Edit `documents.toml` first: remove every `[[root]]` whose space is `name`.
   If the file edit fails, abort — nothing destroyed.
2. `brain.db` (write lock): redact with a new `RedactTarget::Space { id }` —
   the closure is every episode in the space and everything derived from them
   (assertions, statements, mentions, extractions, summaries), reusing the
   existing `RedactionClosure` and audit machinery
   (`crates/oxibrain-store/src/redaction.rs`, `crates/oxibrain-core/src/security.rs:138`).
3. `documents.db` (its lock): delete `documents`, `doc_chunks`,
   `doc_fts_word`, `doc_fts_ngram`, `doc_vectors`, `doc_roots` rows for the
   space.
4. Drop the `spaces` row.
5. **Never delete vault contents** (§1.4 — oxibrain never owns authoring).
   If `~/.oxi/vault/<name>/` is empty, remove the directory; otherwise print
   its path and leave it for the user.

No cross-store transaction exists (P8 is per-store); a crash mid-purge is
recovered by re-running `space remove <name> --purge` — every step is
idempotent. Purge prints the closure summary (episode/assertion/… counts) on
completion. Legacy parent-root `exclude` entries gained in §4.3 are
deliberately kept: if the space is ever re-added, the fixup must not
double-index.

### 4.6 MCP integration

- `serve --stdio` / `serve --http` load `UserConfig` once at startup;
  `ServerState` gains `default_space: String`. The tool-dispatch fallback
  `unwrap_or("personal")` becomes `unwrap_or(&state.default_space)`.
- Scoped sessions are unchanged: membership is checked against the *resolved*
  space; a default the token is not scoped to still yields `UNAUTHORIZED`.
- `spaces/list` and the `spaces://` resource unchanged (enumeration only).

### 4.7 Error taxonomy

New `BrainError` variants (typed, per code-style; no anyhow across crate
boundaries):

- `SpaceNotFound { name: String }`
- `SpaceRemoveRefused { name: String, reasons: Vec<String> }` (empty-check
  failures, is-default)
- `SpaceNameInvalid { name: String, reason: String }`

CLI maps them to the messages above; MCP maps `SpaceNotFound` to a tool error
with the `space add` hint.

## 5. Behavior changes & compatibility

Consumption Contract bumps to **1.5** (`doc/CONSUMPTION_CONTRACT.md`):

1. **Breaking (minor):** verbs no longer implicitly create spaces. Scripts
   that relied on `ingest --space new-space` auto-creating must call
   `oxibrain space add new-space` first (or `init` once).
2. **Additive:** MCP tools omitting `space` now resolve the default from
   `~/.oxi/config.toml` instead of a hardcoded `"personal"`.
3. **Additive:** new CLI verbs `space add|remove|default`; `spaces` listing
   gains columns. The fifteen-tool MCP cap is untouched (no new tools).

Workspace version `0.8.0 → 0.9.0`.

## 6. Testing

- **Config:** resolution order property (`--space` beats file beats built-in);
  missing file; malformed file hard error; unknown keys ignored;
  `set_default_space` preserves comments/unknown keys (toml_edit round-trip).
- **Add/provisioning:** idempotent re-add; vault dir created; root appended
  once; alias collision error; parent fixup adds `exclude` exactly once;
  non-default dir provisions nothing (row + hint only); name validation table.
- **Auto-create abolition (regression):** `ingest`/`ask` with an unknown space
  errors and creates no row — the shadow-row test, now on every verb path.
- **Remove matrix:** unknown; is-default; each empty-check refusal
  (episodes / chunks / root reference) individually; empty removal leaves no
  space row and no documents.db residue.
- **Purge:** end-state property — no row in either store references the space;
  an audit entry exists; `documents.toml` roots for the space are gone;
  **vault files on disk are intact** (the §1.4 test); re-run is idempotent;
  truth-reprojection after purge excludes the space (determinism preserved).
- **MCP:** default fallback uses config; scoped session with non-member
  default still `UNAUTHORIZED`; unknown explicit space errors with the hint.
- **Init:** provisioning idempotent; config written only when absent; existing
  documents.toml untouched beyond additive steps.

## 7. Documentation updates

- `doc/ARCHITECTURE.md` → v2.12: §15.1 gains the space lifecycle (managed
  unit, default resolution, provisioning, P5 purge); §16.4 CLI table gains the
  `space` verbs and config note; §18 marks `~/.oxi/config.toml` implemented
  and documents `~/.oxi/vault/<space>/`; MCP §16.2 default-space note.
- `doc/CONSUMPTION_CONTRACT.md` → 1.5 with §5's three entries.
- No schema migration: `spaces` table unchanged; `RedactTarget::Space` is an
  additive serde variant.
