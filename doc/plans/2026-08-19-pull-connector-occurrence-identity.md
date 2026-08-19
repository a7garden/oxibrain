# Pull Connector Occurrence Identity — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `oxibrain sync` from legacy content-hash dedup to event-identity with occurrence chains, so A→B→A creates three distinct events and crash-retry is safe.

**Architecture:** Pure classification in `oxibrain-core::sync` (P9), one batched store read for locator states, facade delegation, and CLI rewrite of the ingest path to use `Brain::ingest_event` with server-derived occurrence IDs.

**Tech Stack:** Rust 2024, rusqlite (via store), blake3 occurrence IDs, existing `IngestAttachment`/`insert_event` from Plan 1.

## Global Constraints

- Rust 2024, clippy `-D warnings`, no bare `unwrap` outside `#[cfg(test)]`.
- mtime, wall-clock, and process-local counters NEVER define identity (§4.2).
- `occurrence_id = H(source_id, locator, predecessor, content_hash)` — the only derivation for pull connectors.
- Legacy episodes (source_id IS NULL) are never re-ingested; they participate in Unchanged classification only.
- The existing `classify_sync` function and `KnownNotes` type remain for backward compatibility — do NOT delete them.
- Tests use `Connection::open_in_memory()` + `migration::ensure_vec_extension()` + `migration::run(&conn)`.

---

### Task 1: Core classification — `classify_event`

**Files:**
- Modify: `crates/oxibrain-core/src/sync.rs`

**Interfaces:**
- Consumes: `SyncFile`, `SyncAction`, `KnownNotes` (existing); `ContentHash` (types.rs).
- Produces: `LocatorState` struct, `classify_event(files, legacy, event_states) -> Vec<SyncAction>`.

- [ ] **Step 1: Write failing tests**

Add to the existing `#[cfg(test)] mod tests` in `crates/oxibrain-core/src/sync.rs`:

```rust
    use super::LocatorState;

    fn locator_state(occ: &str, content: &str) -> LocatorState {
        LocatorState {
            latest_occurrence_id: occ.into(),
            latest_content_hash: content_hash(content),
        }
    }

    #[test]
    fn classify_event_new_when_no_state() {
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &KnownNotes::new(), &HashMap::new());
        assert_eq!(actions, vec![SyncAction::New(file("a.md", "hello", 1))]);
    }

    #[test]
    fn classify_event_unchanged_when_event_hash_matches() {
        let states = HashMap::from([
            ("a.md".to_string(), locator_state("occ1", "hello")),
        ]);
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &KnownNotes::new(), &states);
        assert_eq!(actions, vec![SyncAction::Unchanged("a.md".into())]);
    }

    #[test]
    fn classify_event_modified_when_event_hash_differs() {
        let states = HashMap::from([
            ("a.md".to_string(), locator_state("occ1", "old")),
        ]);
        let files = vec![file("a.md", "new", 2)];
        let actions = classify_event(files, &KnownNotes::new(), &states);
        assert_eq!(actions, vec![SyncAction::Modified(file("a.md", "new", 2))]);
    }

    #[test]
    fn classify_event_unchanged_via_legacy_hash() {
        // No event-path state, but legacy hash matches → Unchanged.
        let mut legacy = KnownNotes::new();
        legacy.insert("a.md".into(), HashSet::from([content_hash("hello")]));
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &legacy, &HashMap::new());
        assert_eq!(actions, vec![SyncAction::Unchanged("a.md".into())]);
    }

    #[test]
    fn classify_event_modified_via_legacy_mismatch() {
        // Legacy knows the path but content changed → Modified (first event-path ingest).
        let mut legacy = KnownNotes::new();
        legacy.insert("a.md".into(), HashSet::from([content_hash("old")]));
        let files = vec![file("a.md", "new", 2)];
        let actions = classify_event(files, &legacy, &HashMap::new());
        assert_eq!(actions, vec![SyncAction::Modified(file("a.md", "new", 2))]);
    }

    #[test]
    fn classify_event_event_state_takes_precedence_over_legacy() {
        // Event state says "old", legacy says "hello" — event state wins.
        let states = HashMap::from([
            ("a.md".to_string(), locator_state("occ1", "old")),
        ]);
        let mut legacy = KnownNotes::new();
        legacy.insert("a.md".into(), HashSet::from([content_hash("hello")]));
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &legacy, &states);
        // Event state hash != file hash → Modified, even though legacy matches.
        assert_eq!(actions, vec![SyncAction::Modified(file("a.md", "hello", 1))]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain-core -- sync::tests::classify_event`
Expected: FAIL — `LocatorState` and `classify_event` not found.

- [ ] **Step 3: Implement**

Add after the existing `classify` function in `crates/oxibrain-core/src/sync.rs`:

```rust
/// State of the latest event-path episode for a locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatorState {
    /// The occurrence_id of the most recent episode for this locator.
    pub latest_occurrence_id: String,
    /// The content hash of that episode.
    pub latest_content_hash: ContentHash,
}

/// Classify scanned files using event-identity state, falling back to legacy
/// content-hash knowledge for locators not yet on the event path.
///
/// Precedence: event_states > legacy > New.
/// Pure and total: every input file appears in exactly one output action.
pub fn classify_event(
    files: Vec<SyncFile>,
    legacy: &KnownNotes,
    event_states: &HashMap<String, LocatorState>,
) -> Vec<SyncAction> {
    files
        .into_iter()
        .map(|f| {
            if let Some(state) = event_states.get(&f.path) {
                if state.latest_content_hash == f.content_hash {
                    SyncAction::Unchanged(f.path)
                } else {
                    SyncAction::Modified(f)
                }
            } else if let Some(hashes) = legacy.get(&f.path) {
                if !hashes.is_empty() && hashes.contains(&f.content_hash) {
                    SyncAction::Unchanged(f.path)
                } else {
                    SyncAction::Modified(f)
                }
            } else {
                SyncAction::New(f)
            }
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-core -- sync::tests::classify_event`
Expected: PASS (6 tests).

- [ ] **Step 5: Re-export from crate root**

In `crates/oxibrain-core/src/lib.rs`, find line 67:

```rust
pub use sync::{KnownNotes, SyncAction, SyncFile, classify as classify_sync};
```

Replace with:

```rust
pub use sync::{
    KnownNotes, LocatorState, SyncAction, SyncFile, classify as classify_sync, classify_event,
};
```

This is required by Task 3, which imports `classify_event` and `LocatorState` from the crate root.

- [ ] **Step 6: Run full crate tests**

Run: `cargo test -p oxibrain-core`
Expected: PASS, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/oxibrain-core/src/sync.rs crates/oxibrain-core/src/lib.rs
git commit -m "feat(sync): classify_event — event-identity classification with legacy fallback"
```

---

### Task 2: Store read — `locator_states`

**Files:**
- Modify: `crates/oxibrain-store/src/ledger.rs`
- Create: `crates/oxibrain-store/tests/locator_states.rs`

**Interfaces:**
- Consumes: `LocatorState` (re-exported from oxibrain-core), schema v10 columns.
- Produces: `pub fn locator_states(conn, space, source_id) -> Result<HashMap<String, LocatorState>, BrainError>`

- [ ] **Step 1: Write failing tests**

Create `crates/oxibrain-store/tests/locator_states.rs`:

```rust
//! locator_states: latest event-path episode per locator for a source.

use oxibrain_core::{SourceRef, TrustTier, content_hash, occurrence_id};
use oxibrain_ports::Timestamp;
use oxibrain_store::{ledger, migration};
use rusqlite::Connection;

fn setup() -> Connection {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    conn
}

fn register_source(conn: &Connection) -> String {
    let src = ledger::SourceRow {
        id: oxibrain_core::source_id("test", "vault"),
        space: "test".into(),
        name: "vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
        created_at: Timestamp(1000),
    };
    ledger::insert_source(conn, &src).unwrap();
    src.id
}

fn ingest_event(
    conn: &Connection,
    source_id: &str,
    locator: &str,
    predecessor: Option<&str>,
    content: &str,
) -> String {
    let ch = content_hash(content);
    let occ = occurrence_id(source_id, locator, predecessor, &ch);
    let att = ledger::IngestAttachment {
        source_id: source_id.into(),
        occurrence_id: occ.clone(),
        accepted_at: Timestamp(2000),
        principal: "test".into(),
        claims_json: "{}".into(),
    };
    let mut ep = oxibrain_core::Episode {
        id: String::new(),
        space: "test".into(),
        seq: 0,
        content_hash: oxibrain_core::ContentHash([0u8; 32]),
        content: content.into(),
        source: SourceRef::Note { path: locator.into() },
        trust: TrustTier::Trusted,
        kind: oxibrain_core::EpisodeKind::Primary,
        occurred_at: Timestamp(2000),
        ingested_at: Timestamp(2000),
        redacted_at: None,
    };
    ledger::insert_event(conn, &mut ep, Some(&att)).unwrap();
    occ
}

#[test]
fn locator_states_returns_latest_per_locator() {
    let conn = setup();
    let src = register_source(&conn);

    let occ1 = ingest_event(&conn, &src, "a.md", None, "version 1");
    let occ2 = ingest_event(&conn, &src, "a.md", Some(&occ1), "version 2");
    ingest_event(&conn, &src, "b.md", None, "other file");

    let states = ledger::locator_states(&conn, "test", &src).unwrap();
    assert_eq!(states.len(), 2);

    let a = &states["a.md"];
    assert_eq!(a.latest_occurrence_id, occ2, "must return latest occurrence");
    assert_eq!(a.latest_content_hash, content_hash("version 2"));

    let b = &states["b.md"];
    assert_eq!(b.latest_content_hash, content_hash("other file"));
}

#[test]
fn locator_states_empty_for_unknown_source() {
    let conn = setup();
    let states = ledger::locator_states(&conn, "test", "nonexistent").unwrap();
    assert!(states.is_empty());
}

#[test]
fn locator_states_excludes_redacted() {
    let conn = setup();
    let src = register_source(&conn);
    ingest_event(&conn, &src, "a.md", None, "content");

    // Redact the episode.
    conn.execute(
        "UPDATE episodes SET redacted_at = 3000 WHERE source_id = ?1",
        rusqlite::params![src],
    )
    .unwrap();

    let states = ledger::locator_states(&conn, "test", &src).unwrap();
    assert!(states.is_empty(), "redacted episodes must be excluded");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain-store --test locator_states`
Expected: FAIL — `locator_states` not found.

- [ ] **Step 3: Implement**

Add to `crates/oxibrain-store/src/ledger.rs` (after `note_hashes_by_path`):

```rust
/// Latest event-path episode state per locator for a source (§4.2 pull mode).
/// One query; decision-free (P9). Redacted episodes are excluded.
/// Returns the most recent (highest seq) episode per source_ref (locator).
pub fn locator_states(
    conn: &Connection,
    space: &str,
    source_id: &str,
) -> Result<HashMap<String, oxibrain_core::sync::LocatorState>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT source_ref, occurrence_id, content_hash FROM episodes
             WHERE space_id = ?1 AND source_id = ?2 AND redacted_at IS NULL
             ORDER BY seq ASC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space, source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sql_err)?;
    let mut out: HashMap<String, oxibrain_core::sync::LocatorState> = HashMap::new();
    for row in rows {
        let (locator, occ, hash) = row.map_err(sql_err)?;
        let mut bytes = [0u8; 32];
        if hash.len() == 32 {
            bytes.copy_from_slice(&hash);
        }
        // ORDER BY seq ASC → last write wins = latest episode.
        out.insert(
            locator,
            oxibrain_core::sync::LocatorState {
                latest_occurrence_id: occ,
                latest_content_hash: ContentHash(bytes),
            },
        );
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-store --test locator_states`
Expected: PASS (3 tests).

- [ ] **Step 5: Run full store tests**

Run: `cargo test -p oxibrain-store`
Expected: PASS, no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/oxibrain-store/src/ledger.rs crates/oxibrain-store/tests/locator_states.rs
git commit -m "feat(store): locator_states — latest event-path episode per locator"
```

---

### Task 3: Facade + CLI sync rewrite

**Files:**
- Modify: `crates/oxibrain/src/lib.rs` — add `Brain::locator_states`
- Modify: `crates/oxibrain-cli/src/cmd/sync.rs` — rewrite to event path

**Interfaces:**
- Consumes: Tasks 1–2, `Brain::ingest_event`, `Brain::ensure_source`, `IngestAttachment`, `occurrence_id`.
- Produces: `oxibrain sync` uses occurrence identity end-to-end.

- [ ] **Step 1: Add facade methods**

In `crates/oxibrain/src/lib.rs`, add after `note_hashes`:

```rust
    /// Latest event-path episode state per locator for a source.
    /// Used by sync to derive occurrence chains (§4.2 pull mode).
    pub async fn locator_states(
        &self,
        space: &str,
        source_id: &str,
    ) -> Result<HashMap<String, oxibrain_core::sync::LocatorState>, BrainError> {
        let space = space.to_string();
        let source_id = source_id.to_string();
        read_op!(self.handle, |conn| ledger::locator_states(conn, &space, &source_id))
    }

    /// Current time from the configured clock. Exposed for callers that need
    /// a Timestamp without going through an ingest method.
    pub fn clock_now(&self) -> Timestamp {
        self.clock.now()
    }
```

Add `use std::collections::HashMap;` if not already imported at the top of lib.rs.

- [ ] **Step 2: Rewrite sync.rs**

Replace the body of `crates/oxibrain-cli/src/cmd/sync.rs` (keep `run`, `SyncReport`, `print_report`, `systemtime_to_timestamp`):

```rust
//! `oxibrain sync <DIR> [--space s]` — vault sync with occurrence identity.
//!
//! Scans DIR recursively for `.md`/`.html` files (oxibrain-connectors),
//! classifies each against the ledger's event-path state for the vault source
//! (`oxibrain_core::classify_event`), and ingests new/modified files via the
//! event path with derived occurrence IDs (§4.2).
//!
//! Occurrence chain: `occurrence_id = H(source_id, locator, predecessor, content_hash)`.
//! A → B → A creates three events because the predecessor differs.
//! Unchanged files are skipped — re-syncing an unchanged tree is a no-op.
//! Legacy episodes (pre-event-identity) participate in Unchanged classification
//! but are never re-ingested.

use anyhow::{Context, bail};
use oxibrain::{Brain, BrainConfig, IngestAttachment, SourceRef, TrustTier};
use oxibrain_connectors::scan_directory;
use oxibrain_core::{
    SyncAction, SyncFile, classify_event, content_hash, occurrence_id,
    sync::LocatorState,
};
use oxibrain_ports::Timestamp;
use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Per-run outcome, returned for programmatic use and printed by the CLI.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub new: Vec<String>,
    pub unchanged: Vec<String>,
    pub modified: Vec<String>,
}

pub async fn run(dir: &Path, root: &Path, space: &str) -> anyhow::Result<()> {
    let report = sync(dir, root, space).await?;
    print_report(&report);
    Ok(())
}

/// Scan, classify, ingest via event path. The locator convention is the file's
/// path relative to the sync root (forward slashes).
pub async fn sync(dir: &Path, root: &Path, space: &str) -> anyhow::Result<SyncReport> {
    if !root.is_dir() {
        bail!("not a directory: {}", root.display());
    }
    let files = scan_directory(root);
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;

    // Register the vault as a pull source. Source name = canonical path.
    let source_name = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let source_id = brain
        .ensure_source(&space_id, &source_name, "document_revision", "pull")
        .await?;

    // Fetch both classification inputs.
    let legacy = brain.note_hashes(&space_id).await?;
    let event_states = brain.locator_states(&space_id, &source_id).await?;

    // Content is dropped after hashing; keep it per path for the ingest pass.
    let mut contents: HashMap<String, (String, Timestamp)> = HashMap::new();
    let sync_files: Vec<SyncFile> = files
        .into_iter()
        .filter_map(|f| {
            let path = f.path.to_str()?.to_string();
            let modified = systemtime_to_timestamp(f.modified);
            let hash = content_hash(&f.content);
            contents.insert(path.clone(), (f.content, modified));
            Some(SyncFile {
                path,
                content_hash: hash,
                modified,
            })
        })
        .collect();

    let mut report = SyncReport::default();
    let now = brain.clock_now();
    for action in classify_event(sync_files, &legacy, &event_states) {
        match action {
            SyncAction::New(f) => {
                ingest_event_one(
                    &brain, &space_id, &source_id, &contents, &event_states, &f, now,
                ).await?;
                report.new.push(f.path);
            }
            SyncAction::Modified(f) => {
                ingest_event_one(
                    &brain, &space_id, &source_id, &contents, &event_states, &f, now,
                ).await?;
                report.modified.push(f.path);
            }
            SyncAction::Unchanged(p) => report.unchanged.push(p),
        }
    }
    Ok(report)
}

async fn ingest_event_one(
    brain: &Brain,
    space_id: &str,
    source_id: &str,
    contents: &HashMap<String, (String, Timestamp)>,
    event_states: &HashMap<String, LocatorState>,
    f: &SyncFile,
    now: Timestamp,
) -> anyhow::Result<()> {
    let (content, occurred_at) = contents
        .get(&f.path)
        .with_context(|| format!("content missing for scanned path {}", f.path))?;

    // Derive occurrence: predecessor is the latest occurrence for this locator.
    let predecessor = event_states
        .get(&f.path)
        .map(|s| s.latest_occurrence_id.as_str());
    let occ = occurrence_id(source_id, &f.path, predecessor, &f.content_hash);

    let attachment = IngestAttachment {
        source_id: source_id.into(),
        occurrence_id: occ,
        accepted_at: now,
        principal: "sync".into(),
        claims_json: "{}".into(),
    };

    brain
        .ingest_event(
            space_id,
            content.clone(),
            SourceRef::Note { path: f.path.clone() },
            TrustTier::Trusted,
            Some(&attachment),
            "vault-sync",
        )
        .await?;
    Ok(())
}

fn systemtime_to_timestamp(t: std::time::SystemTime) -> Timestamp {
    let millis = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Timestamp(millis)
}

fn print_report(report: &SyncReport) {
    if !report.new.is_empty() {
        for p in &report.new {
            println!("  new: {p}");
        }
    }
    if !report.modified.is_empty() {
        for p in &report.modified {
            println!("  modified: {p}");
        }
    }
    println!(
        "sync complete: {} new, {} unchanged, {} modified",
        report.new.len(),
        report.unchanged.len(),
        report.modified.len()
    );
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p oxibrain-cli`
Expected: compiles clean.

- [ ] **Step 4: Commit**

```bash
git add crates/oxibrain/src/lib.rs crates/oxibrain-cli/src/cmd/sync.rs
git commit -m "feat(sync): migrate pull connector to occurrence identity

sync now registers the vault as a pull source, derives occurrence chains
via H(source_id, locator, predecessor, content_hash), and ingests through
the event path. Legacy episodes participate in Unchanged classification
but are never re-ingested."
```

---

### Task 4: E2E tests — occurrence chain semantics

**Files:**
- Modify: `crates/oxibrain-cli/tests/sync.rs`

**Interfaces:**
- Consumes: Task 3's rewritten sync command.
- Produces: proof that A→B→A creates 3 events, re-sync is idempotent, legacy compat holds.

- [ ] **Step 1: Add new tests**

Append to `crates/oxibrain-cli/tests/sync.rs`:

```rust
#[test]
fn sync_aba_creates_three_events() {
    let store = tempdir().expect("store dir");
    let vault = tempdir().expect("vault dir");

    // A
    write_vault_file(vault.path(), "note.md", "version A");
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "sync A failed: {out}");
    assert_eq!(episode_count(store.path()), 1);

    // B
    write_vault_file(vault.path(), "note.md", "version B");
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "sync B failed: {out}");
    assert_eq!(episode_count(store.path()), 2);

    // A again — must create a THIRD event (predecessor differs).
    write_vault_file(vault.path(), "note.md", "version A");
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "sync A2 failed: {out}");
    assert!(
        out.contains("1 modified"),
        "revert to A must be Modified: {out}"
    );
    assert_eq!(
        episode_count(store.path()),
        3,
        "A→B→A must create three distinct events"
    );
}

#[test]
fn sync_idempotent_after_event_migration() {
    let store = tempdir().expect("store dir");
    let vault = tempdir().expect("vault dir");

    write_vault_file(vault.path(), "x.md", "stable content");

    // First sync: new.
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "first sync failed: {out}");
    assert!(out.contains("1 new"), "unexpected: {out}");

    // Second sync: unchanged (event-path state matches).
    let (ok, out) = run_oxibrain(store.path(), &["sync", vault.path().to_str().unwrap()]);
    assert!(ok, "second sync failed: {out}");
    assert!(out.contains("0 new, 1 unchanged, 0 modified"), "unexpected: {out}");
    assert_eq!(episode_count(store.path()), 1);
}
```

- [ ] **Step 2: Run all sync tests**

Run: `cargo test -p oxibrain-cli --test sync`
Expected: ALL PASS (existing tests + 2 new). The existing `sync_is_idempotent_and_detects_modifications` test must still pass — legacy classification handles the first sync (no event state yet), then event-path handles subsequent syncs.

- [ ] **Step 3: Run full workspace**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/oxibrain-cli/tests/sync.rs
git commit -m "test(sync): A→B→A occurrence chain and idempotent event-path re-sync"
```

---

### Task 5: Documentation

**Files:**
- Modify: `doc/ARCHITECTURE.md` (§4.2 pull connector section, if present)

- [ ] **Step 1: Update architecture doc**

Add or update the pull-connector section to document:
- `oxibrain sync` uses occurrence identity with predecessor chains.
- Legacy episodes are classified but never re-ingested.
- Source name = canonical vault path; kind = document_revision; mode = pull.

- [ ] **Step 2: Commit**

```bash
git add doc/ARCHITECTURE.md
git commit -m "docs: pull connector occurrence identity in architecture canon"
```
