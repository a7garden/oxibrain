# Vault Sync (`oxibrain sync`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this
> plan task-by-task inline. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `oxibrain sync <dir> [--space s]` scans a markdown directory and ingests
new/modified files idempotently, with `occurred_at = file mtime` so episode ids are
stable across re-syncs.

**Architecture:** Pure classification in `oxibrain-core::sync` (P9: store fetches, core
decides), one batched store read `ledger::note_hashes_by_path`, a thin facade read
method `Brain::note_hashes`, and a CLI command that sequences
scan (`oxibrain-connectors`) → classify → `ingest_note` per new/modified file.

**Tech Stack:** Rust 2024, clap, tokio, rusqlite (via store), blake3 content hashes.

## Global Constraints

- clippy clean with `-D warnings`; no bare `unwrap` in non-test code (`expect` with
  reason for invariants).
- English comments/commits/doc-comments.
- Time is explicit: `oxibrain_ports::Timestamp`, never a bare `i64` in a signature.
- Store functions fetch only — no decision logic in `oxibrain-store`.
- `oxibrain-core` must not depend on adapter crates (`oxibrain-connectors` et al.).
- Doc changes land in `doc/ARCHITECTURE.md` (§16.4 CLI) with a version-header bump.
- Tests: property test for the pure classifier (conservation), unit tests in-module,
  store integration test in `crates/oxibrain-store/tests/`, CLI end-to-end test via
  `CARGO_BIN_EXE_oxibrain`.
- Commit only the files this plan touches; three pre-existing dirty files
  (`cmd/import_oxios.rs`, `core/extraction.rs`, `llm-http/anthropic.rs`) are the
  user's WIP and stay untouched and uncommitted.

## Supersession decision (recorded)

Modified files append a new episode; the previous episode and its assertions remain
(append-only ledger, P1). No auto-retract in v1. Stale claims surface via
`oxibrain contradictions`; manual `retract` when needed. Revisit only if the
contradiction list gets noisy in practice.

---

### Task 1: Core classifier `oxibrain-core/src/sync.rs`

**Files:**
- Create: `crates/oxibrain-core/src/sync.rs`
- Modify: `crates/oxibrain-core/src/lib.rs` (module + re-exports)

**Interfaces (produces):**
```rust
pub struct SyncFile { pub path: String, pub content_hash: ContentHash, pub modified: Timestamp }
#[derive(Debug, PartialEq)]
pub enum SyncAction { New(SyncFile), Unchanged(String), Modified(SyncFile) }
pub type KnownNotes = HashMap<String, HashSet<ContentHash>>;
pub fn classify(files: Vec<SyncFile>, known: &KnownNotes) -> Vec<SyncAction>;
```
Rules: `New` iff path absent from `known` or its set is empty; `Unchanged` iff
`known[path]` contains the hash; else `Modified`. Output preserves input order.

- [x] RED: in-module tests — new/unchanged/modified, empty-known-entry → New,
      order preservation, proptest conservation (every file in exactly one action;
      no episode_count change for unchanged).
- [x] Verify RED (`cargo test -p oxibrain-core sync`): panics from `unimplemented!()`.
- [x] GREEN: implement `classify`; re-export from `lib.rs`.
- [x] Verify GREEN: `cargo test -p oxibrain-core sync` passes.

### Task 2: Store read `ledger::note_hashes_by_path`

**Files:**
- Modify: `crates/oxibrain-store/src/ledger.rs`
- Test: `crates/oxibrain-store/tests/ledger.rs`

**Interfaces (produces):**
```rust
pub fn note_hashes_by_path(
    conn: &Connection,
    space: &str,
) -> Result<HashMap<String, HashSet<ContentHash>>, BrainError>;
```
One query: `source_kind = 'note' AND source_ref IS NOT NULL AND redacted_at IS NULL`,
grouped by path. Redacted episodes are not "known" (a re-sync of redacted content
re-ingests).

- [x] RED: test inserts two note episodes (same path, different content) + one
      redacted + one conversation-sourced, asserts the map shape.
- [x] Verify RED: compile error (function missing) is the expected failure mode.
- [x] GREEN: implement; verify test passes.

### Task 3: Facade read method `Brain::note_hashes`

**Files:**
- Modify: `crates/oxibrain/src/lib.rs` (one delegation after `ingest_note`)

**Interfaces:** `pub async fn note_hashes(&self, space: &str) -> Result<KnownNotes, BrainError>`
via `read_op!`. Covered end-to-end by Task 4's test.

### Task 4: CLI `oxibrain sync` + end-to-end test

**Files:**
- Create: `crates/oxibrain-cli/src/cmd/sync.rs` (`pub struct SyncReport`, `pub async fn run`)
- Modify: `crates/oxibrain-cli/src/cli.rs` (Command::Sync), `cmd/mod.rs`, `main.rs`
- Test: `crates/oxibrain-cli/tests/sync.rs` (spawns `CARGO_BIN_EXE_oxibrain`)

Behavior: reject non-directory path; scan; classify; ingest New/Modified with
`occurred_at` = mtime millis (clamped ≥ 0); skip Unchanged. Report line:
`sync complete: {n} new, {u} unchanged, {m} modified` plus per-file lines and a
note on modified paths pointing at `oxibrain contradictions`.

- [x] RED: e2e test — vault with `a.md`, `sub/b.md`; sync → 2 new; sync again →
      0 new / 2 unchanged, episode count unchanged; edit `a.md` → sync → 1 modified;
      sync again → unchanged; `--space` respected (default `personal`).
- [x] Verify RED: `sync` subcommand missing → nonzero exit.
- [x] GREEN: implement command + wiring; verify test passes.

### Task 5: Docs

- [x] Add `oxibrain sync <dir> [--space s]` to `doc/ARCHITECTURE.md` §16.4 with the
      mtime/occurred_at and supersession note; bump the doc version header.

### Task 6: Full verification

- [x] `cargo fmt --all` (write mode), `cargo clippy --all-targets --all-features -- -D warnings`,
      `cargo test --workspace`, standalone guarantee build + tree check.

### Task 7: Commit

- [x] `feat: add vault sync command (idempotent, mtime-anchored)` — only plan files.
