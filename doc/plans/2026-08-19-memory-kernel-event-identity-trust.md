# Memory Kernel P0–P2: Event Identity and Server-Evaluated Trust — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make oxibrain's ingest path carry real provenance: stable source/occurrence identity instead of content-hash identity, and trust assigned by server policy instead of hardcoded `Note` + `Trusted`.

**Architecture:** Episodes stay immutable. A new `IngestAttachment` (source_id, occurrence_id, principal, claims) rides beside the episode on a new write path `ledger::insert_event`; the legacy `insert_episode` path stays for internal callers. Source registration and trust policy become `Declaration` variants so reproject can replay them. The fold stops assuming every episode is `Trusted`: assertions carry their episode's trust and `compute_support` builds real `trust_weights`.

**Tech Stack:** Rust 2024, rusqlite 0.32 (bundled), blake3, serde, tokio, clap. Workspace at `/Volumes/MERCURY/PROJECTS/oxibrain`.

**Spec:** `doc/spec/ecosystem-v2-memory-kernel.md` (v2.0, commit `8e6a8d1`).

## Global Constraints

- Rust 2024; `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all -- --check` must pass.
- No bare `.unwrap()` outside `#[cfg(test)]` code (crate-level `#![cfg_attr(test, allow(clippy::unwrap_used))]` already set in store).
- Public crate APIs return `BrainError` (`crates/oxibrain-ports/src/error.rs`); `anyhow` is CLI-internal only.
- rusqlite errors convert via `.map_err(sql_err)?` — never `?` directly on a rusqlite `Result` in store code (orphan rule; see `crates/oxibrain-store/src/lib.rs:103-109`).
- Episodes are append-only; the only row mutation allowed here is the NULL→value attach of occurrence columns to an existing episode row (idempotent attach, content unchanged).
- Truth-half determinism: every derived id is content-derived; replay order is `seq ASC`; the byte-identical reproject tests (`crates/oxibrain/tests/reproject_determinism.rs`, `crates/oxibrain-store/tests/reproject.rs`) must stay green and must never be weakened.
- Schema changes require a migration file + a migration-chain up-test from the previous version (AGENTS.md).
- The MCP tool surface stays at **fifteen tools** — this plan adds parameters, never tools.
- Comments, doc-comments, and commit messages in English.
- Do not touch `apps/brain-ui` or any oxios/oximemo repository in this plan.

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `crates/oxibrain-core/src/types.rs` | `SourceRef` variants for new source kinds; `TrustTier::ordinal` + `Default` | Modify |
| `crates/oxibrain-core/src/id.rs` | `episode_event_id`, `source_id`, `occurrence_id` derivation | Modify |
| `crates/oxibrain-core/src/security.rs` | `Capability::TrustedIngest`; `Scope.label` | Modify |
| `crates/oxibrain-core/src/knowledge.rs` | `Assertion.trust` field with `#[serde(default)]` | Modify |
| `crates/oxibrain-core/src/fold.rs` | `compute_support` uses real per-episode trust | Modify |
| `crates/oxibrain-store/src/migrations/v10.sql` | episodes table rebuild; `sources`, `source_policies`; `assertions.trust` | Create |
| `crates/oxibrain-store/src/schema.rs` | `LEDGER_SCHEMA_VERSION = 10` | Modify |
| `crates/oxibrain-store/src/migration.rs` | `if current < 10` arm | Modify |
| `crates/oxibrain-store/src/ledger.rs` | `IngestAttachment`, `insert_event`, `decode_source` arms, source/policy CRUD | Modify |
| `crates/oxibrain-store/src/project.rs` | `Declaration::RegisterSource`/`SetSourcePolicy`; assertion trust at insert | Modify |
| `crates/oxibrain-store/src/reproject.rs` | replay the two new declaration kinds | Modify |
| `crates/oxibrain-store/src/knowledge.rs` | assertion insert/read trust column | Modify |
| `crates/oxibrain-store/src/extraction.rs` | `project_extraction` threads episode trust into assertions | Modify |
| `crates/oxibrain/src/ingest.rs` | `ingest_event_impl` | Modify |
| `crates/oxibrain/src/lib.rs` | `Brain::ingest_event` + `Brain::ensure_source` facade | Modify |
| `crates/oxibrain-mcp/src/server.rs` | trust gate in `enforce_scope` + tool schema params | Modify |
| `crates/oxibrain-cli/src/cmd/token.rs` | `Scope` literal gains `label` field | Modify |
| `crates/oxibrain-client/tests/foundation_packages.rs` | `Scope` literal gains `label` field | Modify |
| `crates/oxibrain-store/tests/migration_chain.rs` | v9→v10 chain test | Modify |
| `crates/oxibrain-store/tests/event_identity.rs` | occurrence semantics e2e | Create |
| `crates/oxibrain-store/tests/declarations_meta.rs` | RegisterSource/SetSourcePolicy replay | Create |
| `crates/oxibrain/tests/trust_fold.rs` | trust reaches fold beliefs | Create |
| `crates/oxibrain-cli/tests/serve.rs` | e2e trust gate via daemon | Modify |

---

### Task 1: Core types — new source kinds, trust ordinal, event identity

**Files:**
- Modify: `crates/oxibrain-core/src/types.rs`
- Modify: `crates/oxibrain-core/src/id.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `SourceRef::DocumentRevision { uri }`, `SourceRef::ArtifactEvent { uri }`, `SourceRef::WebClip { uri }`, `SourceRef::CalendarEvent { uri }`; `TrustTier::ordinal()` and `impl Default for TrustTier`; `id::episode_event_id(space, source_id, occurrence_id) -> Id`; `id::source_id(space, name) -> Id`; `id::occurrence_id(source_id, locator, predecessor, content_hash) -> Id`. Later tasks use these exact names.

- [ ] **Step 1: Write failing tests**

Append to `crates/oxibrain-core/src/types.rs` inside the existing `#[cfg(test)] mod tests` block (create the block if absent):

```rust
#[cfg(test)]
mod types_tests {
    use super::*;

    #[test]
    fn new_source_kinds_roundtrip_through_db_columns() {
        let cases = [
            (SourceRef::DocumentRevision { uri: "vault://a.md".into() }, "document_revision"),
            (SourceRef::ArtifactEvent { uri: "oxios://art/1".into() }, "artifact_event"),
            (SourceRef::WebClip { uri: "https://x.test".into() }, "web_clip"),
            (SourceRef::CalendarEvent { uri: "oxiline://evt/1".into() }, "calendar_event"),
        ];
        for (s, kind) in cases {
            let (k, r) = s.db_columns();
            assert_eq!(k, kind);
            assert!(r.is_some());
        }
    }

    #[test]
    fn trust_ordinal_is_total_order() {
        assert!(TrustTier::Trusted.ordinal() < TrustTier::SemiTrusted.ordinal());
        assert!(TrustTier::SemiTrusted.ordinal() < TrustTier::Untrusted.ordinal());
    }

    #[test]
    fn trust_default_is_trusted() {
        assert_eq!(TrustTier::default(), TrustTier::Trusted);
    }
}
```

Append to `crates/oxibrain-core/src/id.rs` inside the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn episode_event_id_is_deterministic_and_distinct_from_episode_id() {
        let a = episode_event_id("sp", "src1", "occ1");
        assert_eq!(a, episode_event_id("sp", "src1", "occ1"));
        assert_ne!(a, episode_event_id("sp", "src1", "occ2"));
        assert_ne!(a, episode_event_id("sp", "src2", "occ1"));
        assert_ne!(a, episode_event_id("sp2", "src1", "occ1"));
    }

    #[test]
    fn source_id_is_deterministic_and_name_sensitive() {
        let a = source_id("sp", "oximemo-vault");
        assert_eq!(a, source_id("sp", "oximemo-vault"));
        assert_ne!(a, source_id("sp", "other"));
        assert_ne!(a, source_id("sp2", "oximemo-vault"));
    }

    #[test]
    fn occurrence_id_depends_on_predecessor_not_clock() {
        let ch = ContentHash([7u8; 32]);
        let first = occurrence_id("src1", "notes/a.md", None, &ch);
        let again = occurrence_id("src1", "notes/a.md", None, &ch);
        let child = occurrence_id("src1", "notes/a.md", Some(&first), &ch);
        assert_eq!(first, again, "same inputs must regenerate the same id");
        assert_ne!(first, child, "predecessor changes identity (A->B->A support)");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain-core types_tests episode_event_id source_id occurrence_id`
Expected: FAIL — unknown variants/functions.

- [ ] **Step 3: Implement**

In `crates/oxibrain-core/src/types.rs`, extend the `SourceRef` enum (after `AgentTrace`, before `Declaration`):

```rust
pub enum SourceRef {
    Note { path: String },
    Document { uri: String },
    DocumentRevision { uri: String },
    Conversation,
    Message,
    AgentTrace,
    ArtifactEvent { uri: String },
    WebClip { uri: String },
    CalendarEvent { uri: String },
    Declaration,
    Derived { of: String },
}
```

In `db_columns` add arms (after the `AgentTrace` arm):

```rust
            Self::DocumentRevision { uri } => ("document_revision", Some(uri.clone())),
            Self::ArtifactEvent { uri } => ("artifact_event", Some(uri.clone())),
            Self::WebClip { uri } => ("web_clip", Some(uri.clone())),
            Self::CalendarEvent { uri } => ("calendar_event", Some(uri.clone())),
```

Add to `impl TrustTier` (after `parse_db`):

```rust
    /// Total order for deterministic sorting (Trusted < SemiTrusted < Untrusted).
    pub fn ordinal(&self) -> u8 {
        match self {
            Self::Trusted => 0,
            Self::SemiTrusted => 1,
            Self::Untrusted => 2,
        }
    }
```

Add `Default` impl (after the `impl TrustTier` block):

```rust
impl Default for TrustTier {
    fn default() -> Self {
        Self::Trusted
    }
}
```

In `crates/oxibrain-core/src/id.rs`, append after `token_id`:

```rust
/// `EpisodeEventId = blake3(space, source_id, occurrence_id)`.
/// This is the event-identity key (§4.2): two independent sources with
/// identical bytes produce distinct episodes because their source_id differs.
pub fn episode_event_id(space: &str, source_id: &str, occurrence_id: &str) -> Id {
    hex(derive(&[
        ("space", space),
        ("source_id", source_id),
        ("occurrence_id", occurrence_id),
    ]))
}

/// `SourceId = blake3(space, name)`. Deterministic; re-registration is idempotent.
pub fn source_id(space: &str, name: &str) -> Id {
    hex(derive(&[("space", space), ("name", name)]))
}

/// `OccurrenceId = blake3(source_id, locator, predecessor, content_hash)`.
/// Server-derived for pull connectors; regenerable after a crash before the
/// cursor advances. mtime and wall clock are deliberately absent (§4.2).
pub fn occurrence_id(
    source_id: &str,
    locator: &str,
    predecessor: Option<&str>,
    content_hash: &ContentHash,
) -> Id {
    hex(derive(&[
        ("source_id", source_id),
        ("locator", locator),
        ("predecessor", predecessor.unwrap_or("")),
        ("content_hash", &content_hash.hex()),
    ]))
}
```

Ensure `episode_event_id`, `source_id`, and `occurrence_id` are re-exported from `crates/oxibrain-core/src/lib.rs` alongside the existing `episode_id`, `content_hash`, etc. Check the existing `pub use` or `pub mod id;` pattern and follow it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-core`
Expected: PASS (all core tests, including existing ones).

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-core/src/types.rs crates/oxibrain-core/src/id.rs crates/oxibrain-core/src/lib.rs
git commit -m "feat(core): add event-identity source kinds, trust ordinal, and occurrence id derivation"
```

---

### Task 2: Schema v10 — episodes rebuild, sources, policies, assertion trust

**Files:**
- Create: `crates/oxibrain-store/src/migrations/v10.sql`
- Modify: `crates/oxibrain-store/src/schema.rs`
- Modify: `crates/oxibrain-store/src/migration.rs`
- Modify: `crates/oxibrain-store/tests/migration_chain.rs`

**Interfaces:**
- Consumes: Task 1.
- Produces: episodes table with 20 columns (15 existing + source_id, occurrence_id, accepted_at, principal, claims_json) and **no** `UNIQUE(space_id, content_hash)` constraint; tables `sources`, `source_policies`; column `assertions.trust`; `LEDGER_SCHEMA_VERSION == 10`. Task 3 builds the CRUD over exactly these names.

**Critical design note:** The v1 schema has `UNIQUE(space_id, content_hash)` on episodes. This conflates byte-equality with event identity — two independent sources containing identical text would be deduplicated into one episode, destroying provenance. The ONLY fix is a table rebuild that drops this constraint. ALTER TABLE cannot drop a UNIQUE constraint in SQLite.

- [ ] **Step 1: Write the migration**

`crates/oxibrain-store/src/migrations/v10.sql`:

```sql
-- v10: source registry, trust policies, event identity, assertion trust.
--
-- Episodes table rebuild: drops UNIQUE(space_id, content_hash) which
-- conflated byte-equality with event identity (§4.2). Two independent
-- sources containing identical bytes are now two independent episodes.
-- The 12-step SQLite ALTER TABLE pattern is used because SQLite cannot
-- drop a UNIQUE constraint via ALTER TABLE.

PRAGMA foreign_keys=OFF;

-- Source registry (created before episodes_new so the FK resolves).
CREATE TABLE IF NOT EXISTS sources (
  id          TEXT PRIMARY KEY,
  space_id    TEXT NOT NULL REFERENCES spaces(id),
  name        TEXT NOT NULL,
  kind        TEXT NOT NULL,
  mode        TEXT NOT NULL CHECK (mode IN ('push', 'pull')),
  claims_json TEXT NOT NULL DEFAULT '{}',
  created_at  INTEGER NOT NULL,
  UNIQUE (space_id, name)
);

-- Trust policies.
CREATE TABLE IF NOT EXISTS source_policies (
  id             TEXT PRIMARY KEY,
  source_id      TEXT NOT NULL REFERENCES sources(id),
  trust          TEXT NOT NULL CHECK (trust IN ('trusted', 'semi_trusted', 'untrusted')),
  effective_from INTEGER NOT NULL,
  effective_to   INTEGER,
  declaration_ep TEXT NOT NULL REFERENCES episodes(id),
  created_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_policy_source ON source_policies(source_id, effective_from);

-- Rebuild episodes without UNIQUE(space_id, content_hash).
CREATE TABLE episodes_new (
  id                TEXT PRIMARY KEY,
  space_id          TEXT NOT NULL REFERENCES spaces(id),
  seq               INTEGER NOT NULL,
  content_hash      BLOB NOT NULL,
  content           TEXT NOT NULL,
  source_kind       TEXT NOT NULL,
  source_ref        TEXT,
  trust             TEXT NOT NULL,
  kind              TEXT NOT NULL,
  occurred_at       INTEGER NOT NULL,
  ingested_at       INTEGER NOT NULL,
  redacted_at       INTEGER,
  content_compacted BLOB,
  compacted_at      INTEGER,
  uncertainty_json  TEXT,
  source_id         TEXT REFERENCES sources(id),
  occurrence_id     TEXT,
  accepted_at       INTEGER,
  principal         TEXT,
  claims_json       TEXT,
  UNIQUE (space_id, seq)
);

INSERT INTO episodes_new
  (id, space_id, seq, content_hash, content, source_kind, source_ref,
   trust, kind, occurred_at, ingested_at, redacted_at,
   content_compacted, compacted_at, uncertainty_json)
SELECT id, space_id, seq, content_hash, content, source_kind, source_ref,
       trust, kind, occurred_at, ingested_at, redacted_at,
       content_compacted, compacted_at, uncertainty_json
FROM episodes;

DROP TABLE episodes;

ALTER TABLE episodes_new RENAME TO episodes;

-- Partial unique index: event identity for new-path episodes only.
-- Legacy episodes (source_id IS NULL) are not constrained by this index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_ep_occurrence
  ON episodes(space_id, source_id, occurrence_id)
  WHERE source_id IS NOT NULL AND occurrence_id IS NOT NULL;

-- Assertion trust: which trust tier the supporting episode had at ingest.
ALTER TABLE assertions ADD COLUMN trust TEXT NOT NULL DEFAULT 'trusted';

PRAGMA foreign_keys=ON;
```

- [ ] **Step 2: Write failing chain test**

Append to `crates/oxibrain-store/tests/migration_chain.rs`:

```rust
// ── v9 → current (v10 event identity) ─────────────────────────────────────

/// Build a v9 database with one space and one legacy episode.
fn build_v9_fixture(conn: &Connection) {
    migration::ensure_vec_extension();
    conn.execute_batch(include_str!("../src/migrations/v1.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v2.sql")).unwrap();
    registry::seed_core_v1(conn).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v3.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v4.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v5.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v6.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v7.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v8.sql")).unwrap();
    conn.execute_batch(include_str!("../src/migrations/v9.sql")).unwrap();
    conn.pragma_update(None, "user_version", 9i64).unwrap();
    insert_test_data(conn);
}

#[test]
fn migrates_from_v9_with_data() {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    build_v9_fixture(&conn);

    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    assert_eq!(LEDGER_SCHEMA_VERSION, 10);

    // New tables exist.
    assert!(has_table(&conn, "sources"));
    assert!(has_table(&conn, "source_policies"));

    // New episode columns exist.
    for col in ["source_id", "occurrence_id", "accepted_at", "principal", "claims_json"] {
        assert!(has_column(&conn, "episodes", col), "episodes.{col} missing");
    }

    // Legacy episode survived with NULL attachment.
    let legacy: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE id = 'ep1' AND source_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy, 1, "legacy episode must survive with NULL attachment");

    // The old UNIQUE(space_id, content_hash) constraint is gone:
    // inserting a second episode with the same content_hash but different
    // source_id must succeed.
    conn.execute(
        "INSERT INTO episodes
         (id, space_id, seq, content_hash, content, source_kind, source_ref,
          trust, kind, occurred_at, ingested_at, source_id, occurrence_id)
         VALUES ('ep_dup', 'sp1', 1, x'00', 'test content', 'note', 'other.md',
                 'trusted', 'primary', 1000, 1000, 'src_x', 'occ_x')",
        [],
    )
    .expect("same content_hash, different source must not conflict after v10");

    // Assertions trust column exists with default.
    assert!(has_column(&conn, "assertions", "trust"));
}
```

- [ ] **Step 3: Wire the migration runner**

`crates/oxibrain-store/src/schema.rs` — change the constant:

```rust
pub const LEDGER_SCHEMA_VERSION: i64 = 10;
```

In `crates/oxibrain-store/src/migration.rs`, after the `if current < 9 { … }` block, append:

```rust
    if current < 10 {
        let sql = include_str!("migrations/v10.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 10i64)
            .map_err(sql_err)?;
    }
```

In the `fresh_db_migrates_to_current` test (inside `mod tests` in migration.rs), after the `uncertainty_json` spot-check, append:

```rust
        // v10: event identity columns exist on episodes
        for col in ["source_id", "occurrence_id", "accepted_at", "principal", "claims_json"] {
            let has: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('episodes') WHERE name = '{col}'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .expect("pragma query");
            assert_eq!(has, 1, "episodes.{col} should exist after v10");
        }
        let has_trust: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('assertions') WHERE name = 'trust'",
                [],
                |r| r.get(0),
            )
            .expect("pragma query");
        assert_eq!(has_trust, 1, "assertions.trust should exist after v10");
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-store migration`
Expected: PASS — fresh db reaches 10; v9 fixture upgrades with data intact; duplicate content_hash with different source_id succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-store/src/migrations/v10.sql crates/oxibrain-store/src/schema.rs \
        crates/oxibrain-store/src/migration.rs crates/oxibrain-store/tests/migration_chain.rs
git commit -m "feat(store): schema v10 — episodes rebuild dropping content-hash uniqueness, sources, trust policies"
```

---

### Task 3: Ledger event path — attachment, insert_event, source/policy CRUD

**Files:**
- Modify: `crates/oxibrain-store/src/ledger.rs`
- Create: `crates/oxibrain-store/tests/event_identity.rs`

**Interfaces:**
- Consumes: Tasks 1–2; existing `insert_episode`, `next_seq`, `sql_err`.
- Produces (exact signatures later tasks call):

```rust
pub struct IngestAttachment {
    pub source_id: String,
    pub occurrence_id: String,
    pub accepted_at: Timestamp,
    pub principal: String,
    pub claims_json: String,
}

pub fn insert_event(
    conn: &Connection,
    ep: &mut Episode,
    attachment: Option<&IngestAttachment>,
) -> Result<(), BrainError>

pub struct SourceRow {
    pub id: String,
    pub space: String,
    pub name: String,
    pub kind: String,
    pub mode: String,
    pub claims_json: String,
    pub created_at: Timestamp,
}

pub fn insert_source(conn: &Connection, row: &SourceRow) -> Result<(), BrainError>
pub fn get_source_by_name(conn: &Connection, space: &str, name: &str) -> Result<Option<SourceRow>, BrainError>
pub fn list_sources(conn: &Connection, space: &str) -> Result<Vec<SourceRow>, BrainError>

pub struct PolicyRow {
    pub id: String,
    pub source_id: String,
    pub trust: TrustTier,
    pub effective_from: Timestamp,
    pub effective_to: Option<Timestamp>,
    pub declaration_ep: String,
    pub created_at: Timestamp,
}

pub fn insert_policy(conn: &Connection, row: &PolicyRow) -> Result<(), BrainError>
pub fn effective_policy_trust(
    conn: &Connection,
    source_id: &str,
    at: Timestamp,
) -> Result<Option<TrustTier>, BrainError>
```

**Semantics of `insert_event`:**
- `attachment == None` → delegate to `insert_episode` (legacy content-hash dedup).
- With attachment: look up by `(space_id, source_id, occurrence_id)`.
  - Row exists, same `content_hash` → no-op, fill `ep.id/seq/content_hash`.
  - Row exists, different `content_hash` → `BrainError::Conflict`.
  - No row → INSERT with all 17 columns (12 base + 5 attachment).

**Critical:** `insert_event` must NOT delegate to `insert_episode` for the attachment path. `insert_episode`'s dedup returns `Ok(())` on existing rows without reporting whether it was a no-op, making UNIQUE-error branching dead code.

- [ ] **Step 1: Write failing tests**

Create `crates/oxibrain-store/tests/event_identity.rs`:

```rust
//! Event identity semantics: occurrence-based dedup, conflict detection,
//! and independence from content-hash dedup.

use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier, content_hash};
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

fn base_episode(space: &str, content: &str) -> Episode {
    Episode {
        id: String::new(),
        space: space.into(),
        seq: 0,
        content_hash: ContentHash([0u8; 32]),
        content: content.into(),
        source: SourceRef::Note { path: "test.md".into() },
        trust: TrustTier::Trusted,
        kind: EpisodeKind::Primary,
        occurred_at: Timestamp(2000),
        ingested_at: Timestamp(2000),
        redacted_at: None,
    }
}

fn att(source_id: &str, occurrence_id: &str) -> ledger::IngestAttachment {
    ledger::IngestAttachment {
        source_id: source_id.into(),
        occurrence_id: occurrence_id.into(),
        accepted_at: Timestamp(3000),
        principal: "test-principal".into(),
        claims_json: "{}".into(),
    }
}

#[test]
fn same_occurrence_same_bytes_is_idempotent() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let mut ep1 = base_episode(&space_id, "hello");
    ledger::insert_event(&conn, &mut ep1, Some(&att("src1", "occ1"))).unwrap();
    let id1 = ep1.id.clone();

    let mut ep2 = base_episode(&space_id, "hello");
    ledger::insert_event(&conn, &mut ep2, Some(&att("src1", "occ1"))).unwrap();
    assert_eq!(ep2.id, id1, "same occurrence + same bytes = idempotent");
}

#[test]
fn same_occurrence_different_bytes_is_conflict() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let mut ep1 = base_episode(&space_id, "hello");
    ledger::insert_event(&conn, &mut ep1, Some(&att("src1", "occ1"))).unwrap();

    let mut ep2 = base_episode(&space_id, "different content");
    let err = ledger::insert_event(&conn, &mut ep2, Some(&att("src1", "occ1"))).unwrap_err();
    assert!(matches!(err, oxibrain_ports::BrainError::Conflict(_)));
}

#[test]
fn same_bytes_different_source_creates_two_episodes() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let mut ep1 = base_episode(&space_id, "identical bytes");
    ledger::insert_event(&conn, &mut ep1, Some(&att("src_a", "occ_a"))).unwrap();

    let mut ep2 = base_episode(&space_id, "identical bytes");
    ledger::insert_event(&conn, &mut ep2, Some(&att("src_b", "occ_b"))).unwrap();

    assert_ne!(ep1.id, ep2.id, "same bytes from different sources = two episodes");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn no_attachment_delegates_to_legacy_path() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let mut ep1 = base_episode(&space_id, "legacy content");
    ledger::insert_event(&conn, &mut ep1, None).unwrap();

    // Same content again → legacy content-hash dedup (no-op).
    let mut ep2 = base_episode(&space_id, "legacy content");
    ledger::insert_event(&conn, &mut ep2, None).unwrap();
    assert_eq!(ep1.id, ep2.id, "legacy path deduplicates by content hash");
}

#[test]
fn source_crud_roundtrip() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let row = ledger::SourceRow {
        id: oxibrain_core::source_id(&space_id, "my-vault"),
        space: space_id.clone(),
        name: "my-vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
        created_at: Timestamp(1000),
    };
    ledger::insert_source(&conn, &row).unwrap();

    // Idempotent re-insert.
    ledger::insert_source(&conn, &row).unwrap();

    let found = ledger::get_source_by_name(&conn, &space_id, "my-vault").unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, row.id);

    let all = ledger::list_sources(&conn, &space_id).unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn policy_trust_lookup_respects_effective_interval() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let src = ledger::SourceRow {
        id: "src_p".into(),
        space: space_id,
        name: "policy-src".into(),
        kind: "note".into(),
        mode: "push".into(),
        claims_json: "{}".into(),
        created_at: Timestamp(1000),
    };
    ledger::insert_source(&conn, &src).unwrap();

    let pol = ledger::PolicyRow {
        id: "pol1".into(),
        source_id: "src_p".into(),
        trust: TrustTier::SemiTrusted,
        effective_from: Timestamp(100),
        effective_to: Some(Timestamp(500)),
        declaration_ep: "ep_decl".into(),
        created_at: Timestamp(100),
    };
    ledger::insert_policy(&conn, &pol).unwrap();

    // Inside interval.
    let t = ledger::effective_policy_trust(&conn, "src_p", Timestamp(200)).unwrap();
    assert_eq!(t, Some(TrustTier::SemiTrusted));

    // Outside interval.
    let t = ledger::effective_policy_trust(&conn, "src_p", Timestamp(600)).unwrap();
    assert_eq!(t, None);

    // No policy at all for unknown source.
    let t = ledger::effective_policy_trust(&conn, "unknown", Timestamp(200)).unwrap();
    assert_eq!(t, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain-store --test event_identity`
Expected: FAIL — `insert_event`, `IngestAttachment`, etc. not found.

- [ ] **Step 3: Implement**

In `crates/oxibrain-store/src/ledger.rs`, add imports at the top:

```rust
use oxibrain_core::id::episode_event_id;
use rusqlite::OptionalExtension;
```

Add after `insert_episode` (line ~108):

```rust
/// Attachment metadata for the event-identity write path (§4.1).
/// All fields are server-assigned; never accepted from client payloads.
pub struct IngestAttachment {
    pub source_id: String,
    pub occurrence_id: String,
    pub accepted_at: Timestamp,
    pub principal: String,
    pub claims_json: String,
}

/// Insert an episode via event identity (§4.2).
///
/// With `attachment`: identity is `(space_id, source_id, occurrence_id)`.
/// Without: delegates to `insert_episode` (legacy content-hash dedup).
///
/// Idempotent: re-inserting the same occurrence with identical bytes is a
/// no-op. Same occurrence with different bytes is `BrainError::Conflict`.
pub fn insert_event(
    conn: &Connection,
    ep: &mut Episode,
    attachment: Option<&IngestAttachment>,
) -> Result<(), BrainError> {
    let Some(att) = attachment else {
        return insert_episode(conn, ep);
    };

    let ch = content_hash(&ep.content);
    let id = episode_event_id(&ep.space, &att.source_id, &att.occurrence_id);

    // Check for existing episode with same event identity.
    let existing: Option<(String, i64, Vec<u8>)> = conn
        .query_row(
            "SELECT id, seq, content_hash FROM episodes
             WHERE space_id = ?1 AND source_id = ?2 AND occurrence_id = ?3",
            params![ep.space, att.source_id, att.occurrence_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql_err)?;

    match existing {
        Some((eid, seq, stored_hash)) => {
            if stored_hash == ch.as_bytes() {
                // Idempotent: same event identity, same bytes.
                ep.id = eid;
                ep.seq = seq as u64;
                ep.content_hash = ch;
                return Ok(());
            }
            Err(BrainError::Conflict(format!(
                "occurrence '{}' already exists with different content",
                att.occurrence_id
            )))
        }
        None => {
            let seq = next_seq(conn, &ep.space)?;
            let (source_kind, source_ref) = ep.source.db_columns();
            conn.execute(
                "INSERT INTO episodes
                 (id, space_id, seq, content_hash, content, source_kind, source_ref,
                  trust, kind, occurred_at, ingested_at, redacted_at,
                  source_id, occurrence_id, accepted_at, principal, claims_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16, ?17)",
                params![
                    id,
                    ep.space,
                    seq,
                    ch.as_bytes(),
                    ep.content,
                    source_kind,
                    source_ref,
                    ep.trust.as_db(),
                    ep.kind.as_db(),
                    ep.occurred_at.millis(),
                    ep.ingested_at.millis(),
                    ep.redacted_at.map(|t| t.millis()),
                    att.source_id,
                    att.occurrence_id,
                    att.accepted_at.millis(),
                    att.principal,
                    att.claims_json,
                ],
            )
            .map_err(sql_err)?;
            ep.id = id;
            ep.seq = seq;
            ep.content_hash = ch;
            Ok(())
        }
    }
}
```

Add `decode_source` arms (inside the existing `fn decode_source`, after the `"agent_trace"` arm):

```rust
        "document_revision" => Ok(SourceRef::DocumentRevision {
            uri: r#ref.unwrap_or_default(),
        }),
        "artifact_event" => Ok(SourceRef::ArtifactEvent {
            uri: r#ref.unwrap_or_default(),
        }),
        "web_clip" => Ok(SourceRef::WebClip {
            uri: r#ref.unwrap_or_default(),
        }),
        "calendar_event" => Ok(SourceRef::CalendarEvent {
            uri: r#ref.unwrap_or_default(),
        }),
```

Add source/policy CRUD after `decode_source`:

```rust
// ── Source registry CRUD ────────────────────────────────────────────────────

/// A registered source row.
pub struct SourceRow {
    pub id: String,
    pub space: String,
    pub name: String,
    pub kind: String,
    pub mode: String,
    pub claims_json: String,
    pub created_at: Timestamp,
}

/// Insert a source (idempotent: same id → no-op).
pub fn insert_source(conn: &Connection, row: &SourceRow) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO sources (id, space_id, name, kind, mode, claims_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.id,
            row.space,
            row.name,
            row.kind,
            row.mode,
            row.claims_json,
            row.created_at.millis(),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Look up a source by (space, name).
pub fn get_source_by_name(
    conn: &Connection,
    space: &str,
    name: &str,
) -> Result<Option<SourceRow>, BrainError> {
    conn.query_row(
        "SELECT id, space_id, name, kind, mode, claims_json, created_at
         FROM sources WHERE space_id = ?1 AND name = ?2",
        params![space, name],
        |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                space: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                mode: r.get(4)?,
                claims_json: r.get(5)?,
                created_at: Timestamp(r.get::<_, i64>(6)?),
            })
        },
    )
    .optional()
    .map_err(sql_err)
}

/// List all sources in a space.
pub fn list_sources(conn: &Connection, space: &str) -> Result<Vec<SourceRow>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, space_id, name, kind, mode, claims_json, created_at
             FROM sources WHERE space_id = ?1 ORDER BY name",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![space], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                space: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                mode: r.get(4)?,
                claims_json: r.get(5)?,
                created_at: Timestamp(r.get::<_, i64>(6)?),
            })
        })
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

// ── Trust policy CRUD ───────────────────────────────────────────────────────

/// A source trust policy row.
pub struct PolicyRow {
    pub id: String,
    pub source_id: String,
    pub trust: TrustTier,
    pub effective_from: Timestamp,
    pub effective_to: Option<Timestamp>,
    pub declaration_ep: String,
    pub created_at: Timestamp,
}

/// Insert a policy (idempotent: same id → no-op).
pub fn insert_policy(conn: &Connection, row: &PolicyRow) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO source_policies
         (id, source_id, trust, effective_from, effective_to, declaration_ep, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.id,
            row.source_id,
            row.trust.as_db(),
            row.effective_from.millis(),
            row.effective_to.map(|t| t.millis()),
            row.declaration_ep,
            row.created_at.millis(),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Effective trust for a source at a given time: the latest policy whose
/// interval contains `at`. Returns None if no policy covers the instant.
pub fn effective_policy_trust(
    conn: &Connection,
    source_id: &str,
    at: Timestamp,
) -> Result<Option<TrustTier>, BrainError> {
    let trust_s: Option<String> = conn
        .query_row(
            "SELECT trust FROM source_policies
             WHERE source_id = ?1
               AND effective_from <= ?2
               AND (effective_to IS NULL OR effective_to > ?2)
             ORDER BY effective_from DESC
             LIMIT 1",
            params![source_id, at.millis()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    Ok(trust_s.and_then(|s| TrustTier::parse_db(&s)))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-store --test event_identity`
Expected: PASS.

Also run: `cargo test -p oxibrain-store` to verify no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-store/src/ledger.rs crates/oxibrain-store/tests/event_identity.rs
git commit -m "feat(store): event-identity write path with occurrence dedup and source/policy CRUD"
```

---

### Task 4: Declarations — RegisterSource and SetSourcePolicy

**Files:**
- Modify: `crates/oxibrain-store/src/project.rs`
- Modify: `crates/oxibrain-store/src/reproject.rs`
- Create: `crates/oxibrain-store/tests/declarations_meta.rs`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: `Declaration::RegisterSource { … }` and `Declaration::SetSourcePolicy { … }` variants; `project_declaration` handles them; reproject replays them.

- [ ] **Step 1: Write failing test**

Create `crates/oxibrain-store/tests/declarations_meta.rs`:

```rust
//! RegisterSource and SetSourcePolicy declarations project into the
//! sources/source_policies tables and survive reproject.

use oxibrain_core::TrustTier;
use oxibrain_ports::Timestamp;
use oxibrain_store::{ledger, migration, project, reproject};
use oxibrain_store::project::{Declaration, ResolutionCache};
use rusqlite::Connection;

fn setup() -> Connection {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    ledger::create_space(&conn, "test", Timestamp(1000)).unwrap();
    conn
}

#[test]
fn register_source_declaration_creates_source_row() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let decl = Declaration::RegisterSource {
        name: "my-vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
    };
    let mut cache = ResolutionCache::new();
    project::project_declaration(&conn, &space_id, &decl, Timestamp(2000), &mut cache).unwrap();

    let src = ledger::get_source_by_name(&conn, &space_id, "my-vault").unwrap();
    assert!(src.is_some(), "RegisterSource must create a sources row");
    let src = src.unwrap();
    assert_eq!(src.kind, "document_revision");
    assert_eq!(src.mode, "pull");
}

#[test]
fn set_source_policy_declaration_creates_policy_row() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    // First register the source.
    let reg = Declaration::RegisterSource {
        name: "vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
    };
    let mut cache = ResolutionCache::new();
    project::project_declaration(&conn, &space_id, &reg, Timestamp(2000), &mut cache).unwrap();

    // Then set its policy.
    let pol = Declaration::SetSourcePolicy {
        source_name: "vault".into(),
        trust: "semi_trusted".into(),
        effective_from: 1000,
        effective_to: None,
    };
    project::project_declaration(&conn, &space_id, &pol, Timestamp(3000), &mut cache).unwrap();

    let src = ledger::get_source_by_name(&conn, &space_id, "vault").unwrap().unwrap();
    let trust = ledger::effective_policy_trust(&conn, &src.id, Timestamp(2000)).unwrap();
    assert_eq!(trust, Some(TrustTier::SemiTrusted));
}

#[test]
fn meta_declarations_survive_reproject() {
    let conn = setup();
    let space_id: String = conn
        .query_row("SELECT id FROM spaces WHERE name = 'test'", [], |r| r.get(0))
        .unwrap();

    let reg = Declaration::RegisterSource {
        name: "vault".into(),
        kind: "document_revision".into(),
        mode: "pull".into(),
        claims_json: "{}".into(),
    };
    let pol = Declaration::SetSourcePolicy {
        source_name: "vault".into(),
        trust: "trusted".into(),
        effective_from: 0,
        effective_to: None,
    };
    let mut cache = ResolutionCache::new();
    project::project_declaration(&conn, &space_id, &reg, Timestamp(2000), &mut cache).unwrap();
    project::project_declaration(&conn, &space_id, &pol, Timestamp(3000), &mut cache).unwrap();

    // Reproject wipes projection tables and replays.
    reproject::reproject(&conn).unwrap();

    // Source and policy must still exist after replay.
    let src = ledger::get_source_by_name(&conn, &space_id, "vault").unwrap();
    assert!(src.is_some(), "source must survive reproject");
    let src = src.unwrap();
    let trust = ledger::effective_policy_trust(&conn, &src.id, Timestamp(5000)).unwrap();
    assert_eq!(trust, Some(TrustTier::Trusted), "policy must survive reproject");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain-store --test declarations_meta`
Expected: FAIL — `Declaration::RegisterSource` unknown variant.

- [ ] **Step 3: Implement**

In `crates/oxibrain-store/src/project.rs`, add two variants to the `Declaration` enum (after `Retract`):

```rust
    RegisterSource {
        name: String,
        kind: String,
        mode: String,
        claims_json: String,
    },
    SetSourcePolicy {
        source_name: String,
        trust: String,
        effective_from: i64,
        effective_to: Option<i64>,
    },
```

In `project_declaration`, add match arms after the `Retract` arm. The RegisterSource arm:

```rust
        Declaration::RegisterSource {
            name,
            kind,
            mode,
            claims_json,
        } => {
            let src_id = oxibrain_core::source_id(space, name);
            let row = ledger::SourceRow {
                id: src_id,
                space: space.to_string(),
                name: name.clone(),
                kind: kind.clone(),
                mode: mode.clone(),
                claims_json: claims_json.clone(),
                created_at: now,
            };
            ledger::insert_source(conn, &row)?;
        }
```

The SetSourcePolicy arm:

```rust
        Declaration::SetSourcePolicy {
            source_name,
            trust,
            effective_from,
            effective_to,
        } => {
            let tier = TrustTier::parse_db(trust)
                .ok_or_else(|| BrainError::Invalid(format!("bad trust tier: {trust}")))?;
            let src = ledger::get_source_by_name(conn, space, source_name)?
                .ok_or_else(|| BrainError::NotFound(format!("source not found: {source_name}")))?;
            let pol_id = oxibrain_core::id::source_policy_id(&src.id, *effective_from);
            let row = ledger::PolicyRow {
                id: pol_id,
                source_id: src.id,
                trust: tier,
                effective_from: Timestamp(*effective_from),
                effective_to: effective_to.map(Timestamp),
                declaration_ep: ep_id.clone(),
                created_at: now,
            };
            ledger::insert_policy(conn, &row)?;
        }
```

This requires a new id function. Add to `crates/oxibrain-core/src/id.rs`:

```rust
/// `SourcePolicyId = blake3(source_id, effective_from)`.
pub fn source_policy_id(source_id: &str, effective_from: i64) -> Id {
    hex(derive(&[
        ("source_id", source_id),
        ("effective_from", &effective_from.to_string()),
    ]))
}
```

Re-export it alongside the other id functions.

In `project_declaration`, add the necessary imports at the top of the function or at file level: `use crate::ledger;` and `use oxibrain_core::TrustTier;` (check existing imports — `TrustTier` may already be imported via `oxibrain_core::TrustTier`).

In `crates/oxibrain-store/src/reproject.rs`, the replay loop already calls `project_declaration` for every Declaration episode. Since `RegisterSource` and `SetSourcePolicy` are Declaration episodes with `kind = 'declaration'`, they are already picked up by the existing `WHERE kind = 'declaration'` query. **No change needed in reproject.rs** — the existing replay loop handles them automatically because `project_declaration` now handles the new variants.

However, verify that `sources` and `source_policies` are NOT in the wipe list (they shouldn't be — the wipe list is `["beliefs","mentions","assertions","statements","entity_merges","entity_keys","entities"]`). Since they are not wiped, the replay's `INSERT OR IGNORE` makes them idempotent.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-store --test declarations_meta`
Expected: PASS.

Also run: `cargo test -p oxibrain-store` for regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-store/src/project.rs crates/oxibrain-store/tests/declarations_meta.rs \
        crates/oxibrain-core/src/id.rs crates/oxibrain-core/src/lib.rs
git commit -m "feat(store): RegisterSource and SetSourcePolicy declarations with reproject replay"
```

---

### Task 5: Fold trust — Assertion.trust and per-episode trust_weights

**Files:**
- Modify: `crates/oxibrain-core/src/knowledge.rs` (Assertion struct)
- Modify: `crates/oxibrain-core/src/fold.rs` (compute_support + test helpers)
- Modify: `crates/oxibrain-store/src/knowledge.rs` (insert/read trust)
- Modify: `crates/oxibrain-store/src/extraction.rs` (thread episode trust)
- Modify: `crates/oxibrain-store/src/project.rs` (declaration assertions get Trusted)
- Modify: `crates/oxibrain-core/tests/fold_property.rs` (make_assertion helper)
- Delete or handle: `crates/oxibrain-core/tests/fold_property.proptest-regressions`

**Interfaces:**
- Consumes: Tasks 1–2 (TrustTier::Default, assertions.trust column).
- Produces: `Assertion.trust: TrustTier` with `#[serde(default)]`; `compute_support` returns real per-tier counts; all assertion construction sites pass trust.

- [ ] **Step 1: Add trust field to Assertion**

In `crates/oxibrain-core/src/knowledge.rs`, add to the `Assertion` struct (after `retracted_at`):

```rust
    /// Trust tier of the supporting episode at ingest time.
    #[serde(default)]
    pub trust: TrustTier,
```

Add `use crate::types::TrustTier;` if not already imported (check existing imports at top of file — it already has `use crate::types::TrustTier;`).

- [ ] **Step 2: Update compute_support in fold.rs**

Replace the `compute_support` function body:

```rust
/// Compute support from visible assertions.
fn compute_support(assertions: &[Assertion]) -> Support {
    use std::collections::BTreeMap;

    let affirm_count = assertions
        .iter()
        .filter(|a| a.polarity == Polarity::Affirm)
        .count() as u32;
    let deny_count = assertions
        .iter()
        .filter(|a| a.polarity == Polarity::Deny)
        .count() as u32;

    // Distinct episodes per trust tier. Trust is per-episode, not per-assertion,
    // so we deduplicate by episode id first.
    let mut episode_trust: BTreeMap<&str, TrustTier> = BTreeMap::new();
    for a in assertions {
        episode_trust.entry(a.episode.as_str()).or_insert(a.trust);
    }

    let mut trusted = 0u32;
    let mut semi = 0u32;
    let mut untrusted = 0u32;
    for (_, tier) in &episode_trust {
        match tier {
            TrustTier::Trusted => trusted += 1,
            TrustTier::SemiTrusted => semi += 1,
            TrustTier::Untrusted => untrusted += 1,
        }
    }

    let mut trust_weights = Vec::new();
    if trusted > 0 {
        trust_weights.push((TrustTier::Trusted, trusted));
    }
    if semi > 0 {
        trust_weights.push((TrustTier::SemiTrusted, semi));
    }
    if untrusted > 0 {
        trust_weights.push((TrustTier::Untrusted, untrusted));
    }

    Support {
        affirm_count,
        deny_count,
        distinct_episodes: episode_trust.len() as u32,
        trust_weights,
    }
}
```

- [ ] **Step 3: Update fold.rs test helpers**

In `fold.rs` `mod tests`, update `make_assertion` to include `trust: TrustTier::Trusted`:

```rust
    fn make_assertion(
        stmt: &str,
        episode: &str,
        polarity: Polarity,
        from: Timestamp,
        to: Timestamp,
    ) -> Assertion {
        Assertion {
            id: format!("a_{stmt}_{episode}"),
            statement: stmt.into(),
            episode: episode.into(),
            extractor: None,
            polarity,
            claimed_from: from,
            claimed_to: to,
            confidence: 1.0,
            recorded_at: ts(1),
            retracted_at: None,
            trust: TrustTier::Trusted,
        }
    }
```

Find ALL other `Assertion { … }` literal constructions in `fold.rs` tests (there are several: `ext_assertion` around line 547, `deny1` around line 721, `a1` around line 755) and add `trust: TrustTier::Trusted,` to each.

- [ ] **Step 4: Update fold_property.rs**

In `crates/oxibrain-core/tests/fold_property.rs`, update `make_assertion`:

```rust
fn make_assertion(stmt: &str, ep: &str, polarity: Polarity, from: i64, to: i64) -> Assertion {
    Assertion {
        id: format!("a_{stmt}_{ep}"),
        statement: stmt.into(),
        episode: ep.into(),
        extractor: None,
        polarity,
        claimed_from: ts(from),
        claimed_to: ts(to),
        confidence: 1.0,
        recorded_at: ts(1),
        retracted_at: None,
        trust: oxibrain_core::TrustTier::Trusted,
    }
}
```

Also check if there are any other `Assertion { … }` literals in this file and add the trust field.

**Handle the regression file:** The file `crates/oxibrain-core/tests/fold_property.proptest-regressions` contains serialized `Assertion` values without the `trust` field. Since we added `#[serde(default)]` to the field, deserialization will use `TrustTier::default()` (= Trusted). However, the regression file uses Debug format, not serde. **Delete the file** — proptest will regenerate regressions if needed:

```bash
rm crates/oxibrain-core/tests/fold_property.proptest-regressions
```

- [ ] **Step 5: Update store knowledge.rs — insert and read trust**

In `crates/oxibrain-store/src/knowledge.rs`:

Update `insert_assertion` SQL to include trust:

```rust
pub fn insert_assertion(conn: &Connection, a: &Assertion) -> Result<(), BrainError> {
    conn.execute(
        "INSERT OR IGNORE INTO assertions (id, statement_id, episode_id, extractor_id, polarity, claimed_from, claimed_to, confidence, recorded_at, retracted_at, trust)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            a.id,
            a.statement,
            a.episode,
            a.extractor,
            a.polarity.as_db(),
            a.claimed_from.millis(),
            a.claimed_to.millis(),
            a.confidence,
            a.recorded_at.millis(),
            a.retracted_at.map(|t| t.millis()),
            a.trust.as_db(),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}
```

Update `get_assertions_for_statement` to read trust:

```rust
pub fn get_assertions_for_statement(
    conn: &Connection,
    statement_id: &str,
) -> Result<Vec<Assertion>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, statement_id, episode_id, extractor_id, polarity,
                    claimed_from, claimed_to, confidence, recorded_at, retracted_at, trust
             FROM assertions WHERE statement_id = ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![statement_id], |r| {
            let polarity_val: i64 = r.get(4)?;
            let trust_s: String = r.get(10)?;
            Ok(Assertion {
                id: r.get(0)?,
                statement: r.get(1)?,
                episode: r.get(2)?,
                extractor: r.get(3)?,
                polarity: Polarity::parse_db(polarity_val).expect("valid polarity in db"),
                claimed_from: oxibrain_ports::Timestamp(r.get::<_, i64>(5)?),
                claimed_to: oxibrain_ports::Timestamp(r.get::<_, i64>(6)?),
                confidence: r.get(7)?,
                recorded_at: oxibrain_ports::Timestamp(r.get::<_, i64>(8)?),
                retracted_at: r.get::<_, Option<i64>>(9)?.map(oxibrain_ports::Timestamp),
                trust: oxibrain_core::TrustTier::parse_db(&trust_s)
                    .expect("valid trust tier in db"),
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}
```

- [ ] **Step 6: Update extraction.rs — thread episode trust**

In `crates/oxibrain-store/src/extraction.rs`, in `project_extraction`, add a trust lookup before the claim loop (after `let mut count = 0;`):

```rust
    // Look up the episode's trust tier once — all assertions from this
    // episode inherit it (P2: trust is a property of the evidence source).
    let trust_str: String = conn
        .query_row(
            "SELECT trust FROM episodes WHERE id = ?1",
            rusqlite::params![episode_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let episode_trust = oxibrain_core::TrustTier::parse_db(&trust_str)
        .unwrap_or(oxibrain_core::TrustTier::Untrusted);
```

Then in the `Assertion { … }` construction inside the loop (around line 360), add:

```rust
            trust: episode_trust,
```

- [ ] **Step 7: Update project.rs — declaration assertions are Trusted**

In `crates/oxibrain-store/src/project.rs`, in the `AddStatement` arm's `Assertion { … }` construction (around line 558), add:

```rust
                trust: oxibrain_core::TrustTier::Trusted,
```

- [ ] **Step 8: Run all tests**

Run: `cargo test -p oxibrain-core && cargo test -p oxibrain-store`
Expected: PASS. The reproject determinism tests must remain green.

- [ ] **Step 9: Commit**

```bash
git add crates/oxibrain-core/src/knowledge.rs crates/oxibrain-core/src/fold.rs \
        crates/oxibrain-core/tests/fold_property.rs \
        crates/oxibrain-store/src/knowledge.rs crates/oxibrain-store/src/extraction.rs \
        crates/oxibrain-store/src/project.rs
git rm crates/oxibrain-core/tests/fold_property.proptest-regressions
git commit -m "feat(core): assertion trust field with per-episode trust_weights in fold"
```

---

### Task 6: Facade — ingest_event and ensure_source

**Files:**
- Modify: `crates/oxibrain/src/ingest.rs`
- Modify: `crates/oxibrain/src/lib.rs`
- Create: `crates/oxibrain/tests/event_ingest.rs`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces: `Brain::ingest_event(space, content, source, attachment, extractor_id) -> Result<String, BrainError>`; `Brain::ensure_source(space, name, kind, mode) -> Result<String, BrainError>`.

- [ ] **Step 1: Write failing test**

Create `crates/oxibrain/tests/event_ingest.rs`:

```rust
//! Facade-level event ingest: attachment rides through to the ledger.

use oxibrain::{Brain, BrainConfig, SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::ledger::IngestAttachment;

#[tokio::test]
async fn ingest_event_with_attachment_persists_provenance() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("test").await.unwrap();

    let att = IngestAttachment {
        source_id: "src_facade".into(),
        occurrence_id: "occ_facade".into(),
        accepted_at: Timestamp(5000),
        principal: "facade-test".into(),
        claims_json: "{}".into(),
    };

    let ep_id = brain
        .ingest_event(
            &space_id,
            "event content".into(),
            SourceRef::DocumentRevision { uri: "vault://x.md".into() },
            TrustTier::Trusted,
            Some(&att),
            "test-extractor",
        )
        .await
        .unwrap();

    // Verify the episode has the attachment.
    let ep = brain.get_episode(&ep_id).await.unwrap().unwrap();
    assert_eq!(ep.content, "event content");
}

#[tokio::test]
async fn ensure_source_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("test").await.unwrap();

    let id1 = brain.ensure_source(&space_id, "vault", "document_revision", "pull").await.unwrap();
    let id2 = brain.ensure_source(&space_id, "vault", "document_revision", "pull").await.unwrap();
    assert_eq!(id1, id2, "ensure_source must be idempotent");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain --test event_ingest`
Expected: FAIL — `ingest_event` and `ensure_source` not found.

- [ ] **Step 3: Implement**

In `crates/oxibrain/src/ingest.rs`, add after `ingest_impl`:

```rust
    /// Ingest an episode with event-identity attachment. Returns the episode id.
    /// `trust` is the server-evaluated trust tier for this episode.
    pub(crate) async fn ingest_event_impl(
        &self,
        space: &str,
        content: String,
        source: SourceRef,
        trust: TrustTier,
        attachment: Option<oxibrain_store::ledger::IngestAttachment>,
        extractor_id: &str,
    ) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let space = space.to_string();
        let extractor_id = extractor_id.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let ep_id = oxibrain_store::extraction::ingest_event_and_enqueue(
                    conn,
                    &space,
                    &content,
                    source,
                    trust,
                    attachment.as_ref(),
                    &extractor_id,
                    now,
                )?;
                let _ = tx.send(ep_id);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ingest_event channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Ensure a source is registered. Returns its id. Idempotent.
    pub(crate) async fn ensure_source_impl(
        &self,
        space: &str,
        name: &str,
        kind: &str,
        mode: &str,
    ) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let space = space.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mode = mode.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let src_id = oxibrain_core::source_id(&space, &name);
                let row = oxibrain_store::ledger::SourceRow {
                    id: src_id.clone(),
                    space: space.clone(),
                    name,
                    kind,
                    mode,
                    claims_json: "{}".into(),
                    created_at: now,
                };
                oxibrain_store::ledger::insert_source(conn, &row)?;
                let _ = tx.send(src_id);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ensure_source channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
```

This requires a new function in `crates/oxibrain-store/src/extraction.rs`. Add after `ingest_and_enqueue`:

```rust
/// Event-identity variant of `ingest_and_enqueue`. Uses `insert_event` with
/// an optional attachment, then indexes and enqueues extraction.
/// `trust` is the server-evaluated trust tier for this episode.
pub fn ingest_event_and_enqueue(
    conn: &Connection,
    space: &str,
    content: &str,
    source: SourceRef,
    trust: oxibrain_core::TrustTier,
    attachment: Option<&ledger::IngestAttachment>,
    extractor_id: &str,
    now: Timestamp,
) -> Result<String, BrainError> {
    let occurred_at = now;
    let mut episode = oxibrain_core::Episode {
        id: String::new(),
        space: space.into(),
        seq: 0,
        content_hash: oxibrain_core::ContentHash([0u8; 32]),
        content: content.into(),
        source,
        trust,
        kind: EpisodeKind::Primary,
        occurred_at,
        ingested_at: now,
        redacted_at: None,
    };
    ledger::insert_event(conn, &mut episode, attachment)?;
    let ep_id = episode.id.clone();

    crate::index_ops::index_episode_fts(conn, &episode.space, &ep_id, &episode.content)?;
    enqueue_job(conn, &ep_id, extractor_id, now)?;
    Ok(ep_id)
}
```

In `crates/oxibrain/src/lib.rs`, add public facade methods (near the existing `ingest` method):

```rust
    /// Ingest an episode with event-identity provenance (§4.1).
    /// `trust` is the server-evaluated trust tier. `attachment` carries
    /// server-assigned source/occurrence/principal. Pass `None` attachment
    /// for legacy content-hash dedup behavior.
    pub async fn ingest_event(
        &self,
        space: &str,
        content: String,
        source: SourceRef,
        trust: TrustTier,
        attachment: Option<&oxibrain_store::ledger::IngestAttachment>,
        extractor_id: &str,
    ) -> Result<String, BrainError> {
        self.ingest_event_impl(space, content, source, trust, attachment.cloned(), extractor_id)
            .await
    }

    /// Ensure a source is registered in the source registry. Returns its id.
    /// Idempotent: re-registration returns the same id.
    pub async fn ensure_source(
        &self,
        space: &str,
        name: &str,
        kind: &str,
        mode: &str,
    ) -> Result<String, BrainError> {
        self.ensure_source_impl(space, name, kind, mode).await
    }
```

Also re-export `IngestAttachment` from the facade if needed for external consumers:

```rust
pub use oxibrain_store::ledger::IngestAttachment;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain --test event_ingest`
Expected: PASS.

Also run: `cargo test -p oxibrain` for regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain/src/ingest.rs crates/oxibrain/src/lib.rs \
        crates/oxibrain/tests/event_ingest.rs crates/oxibrain-store/src/extraction.rs
git commit -m "feat(facade): ingest_event and ensure_source with event-identity attachment"
```

---

### Task 7: MCP — trust gate, TrustedIngest capability, Scope.label

**Files:**
- Modify: `crates/oxibrain-core/src/security.rs` (Capability::TrustedIngest, Scope.label)
- Modify: `crates/oxibrain-mcp/src/server.rs` (enforce_scope gate, tool schemas)
- Modify: `crates/oxibrain-cli/src/cmd/token.rs` (Scope literal)
- Modify: `crates/oxibrain-client/tests/foundation_packages.rs` (Scope literal)

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces: `Capability::TrustedIngest`; `Scope.label: String` with `#[serde(default)]`; enforce_scope rejects `trust: "trusted"` from tokens lacking TrustedIngest.

- [ ] **Step 1: Add TrustedIngest capability and Scope.label**

In `crates/oxibrain-core/src/security.rs`:

Add to `Capability` enum (after `Redact`):

```rust
    /// May mark ingested content as trusted (bypasses server trust evaluation).
    TrustedIngest,
```

Add to `parse_set` match (after `"redact"` arm):

```rust
                "trusted_ingest" => Some(Capability::TrustedIngest),
```

Add to `as_str` match (after `Redact` arm):

```rust
            Capability::TrustedIngest => "trusted_ingest",
```

Add `label` field to `Scope` struct (after `expires_at`):

```rust
    /// Human-readable label for the token (informational only).
    #[serde(default)]
    pub label: String,
```

- [ ] **Step 2: Fix exhaustive Scope literals**

Three sites construct `Scope` with all fields explicitly (no `..Default::default()`). Add `label: String::new()` to each:

1. `crates/oxibrain-cli/src/cmd/token.rs` line ~13:

```rust
    let scope = Scope {
        spaces: vec![space_id],
        caps: caps_set,
        predicate_filter: None,
        entity_type_filter: None,
        expires_at: None,
        label: String::new(),
    };
```

2. `crates/oxibrain-client/tests/foundation_packages.rs` line ~294:

```rust
    let scope_before = Scope {
        spaces: vec!["personal".to_owned()],
        caps: read_only_caps,
        predicate_filter: None,
        entity_type_filter: None,
        expires_at: None,
        label: String::new(),
    };
```

3. `crates/oxibrain-mcp/src/server.rs` `scope_for` helper (line ~2356):

```rust
    fn scope_for(caps: &[Capability], spaces: &[&str]) -> Scope {
        Scope {
            spaces: spaces.iter().map(|s| s.to_string()).collect(),
            caps: caps.iter().copied().collect(),
            predicate_filter: None,
            entity_type_filter: None,
            expires_at: None,
            label: String::new(),
        }
    }
```

All other Scope construction sites use `..Default::default()` and will pick up `label: String::new()` automatically.

- [ ] **Step 3: Add trust gate to enforce_scope**

In `crates/oxibrain-mcp/src/server.rs`, in `enforce_scope`, after the space membership check (after the `if !scope.spaces.iter().any(...)` block), add:

```rust
        // Trust gate: only tokens with TrustedIngest may claim trust=trusted.
        // Without the capability, the server evaluates trust from policy.
        if matches!(tool, "ingest" | "remember") {
            let requested_trust = args
                .get("trust")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if requested_trust == "trusted" && !scope.caps.contains(&Capability::TrustedIngest) {
                return Err((
                    UNAUTHORIZED,
                    "trust='trusted' requires the trusted_ingest capability".into(),
                ));
            }
        }
```

- [ ] **Step 4: Update tool schemas**

In `tool_list()`, add `"trust"` parameter to the `ingest` tool schema (in the `"properties"` object):

```json
"trust": { "type": "string", "enum": ["trusted","semi_trusted","untrusted"], "description": "Requested trust tier. Requires trusted_ingest capability for 'trusted'. Default: trusted (parity with the note path until the policy engine lands)." }
```

Add the same to the `remember` tool schema.

- [ ] **Step 5: Wire tool_ingest and tool_remember to event path**

In `crates/oxibrain-mcp/src/server.rs`, add two helper methods to `impl BrainServer`:

```rust
    /// Resolve the trust tier for an ingest call. If the caller passed
    /// `trust` and has the TrustedIngest capability (already gated in
    /// enforce_scope), honor it. Otherwise default to Trusted — parity with
    /// the existing `ingest_note` path (ingest.rs uses TrustTier::Trusted).
    /// Plan 2 replaces this default with `effective_policy_trust` lookup.
    fn resolve_trust(&self, args: &Value) -> TrustTier {
        match args.get("trust").and_then(|v| v.as_str()) {
            Some("trusted") => TrustTier::Trusted,
            Some("semi_trusted") => TrustTier::SemiTrusted,
            Some("untrusted") => TrustTier::Untrusted,
            _ => TrustTier::Trusted,
        }
    }

    /// Build an IngestAttachment for MCP-originated content.
    /// occurrence_id = content_hash(content): same content re-push is
    /// idempotent; different content at same locator creates a new episode.
    async fn build_attachment(
        &self,
        space_id: &str,
        source_name: &str,
        content: &str,
        now: Timestamp,
    ) -> Result<oxibrain_store::ledger::IngestAttachment, ToolErr> {
        let source_id = self
            .brain
            .ensure_source(space_id, source_name, "mcp", "push")
            .await
            .map_err(ToolErr::run)?;
        let occurrence_id = oxibrain_core::content_hash(content).hex();
        let principal = self
            .scope
            .as_ref()
            .map(|s| s.label.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "mcp".into());
        Ok(oxibrain_store::ledger::IngestAttachment {
            source_id,
            occurrence_id,
            accepted_at: now,
            principal,
            claims_json: "{}".into(),
        })
    }
```

Rewrite `tool_ingest` to use the event path:

```rust
    async fn tool_ingest(
        &self,
        args: &Value,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Result<String, ToolErr> {
        let content = str_arg(args, "content")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let path = str_arg_or(args, "source_path", "mcp");
        let extract = args
            .get("extract")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let now = SystemClock.now();
        let trust = self.resolve_trust(args);
        let attachment = self
            .build_attachment(&space_id, &path, &content, now)
            .await?;
        let id = self
            .brain
            .ingest_event(
                &space_id,
                content.to_string(),
                SourceRef::Note { path: path.clone() },
                trust,
                Some(&attachment),
                "mcp-ingest",
            )
            .await
            .map_err(ToolErr::run)?;

        if extract {
            return self.try_sample_extract(&space_id, &id, session).await;
        }
        Ok(format!("Ingested as episode: {id}"))
    }
```

Rewrite `tool_remember` similarly:

```rust
    async fn tool_remember(
        &self,
        args: &Value,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Result<String, ToolErr> {
        let content = str_arg(args, "content")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let path = str_arg_or(args, "source_path", "remember");
        let now = SystemClock.now();
        let trust = self.resolve_trust(args);
        let attachment = self
            .build_attachment(&space_id, &path, &content, now)
            .await?;
        let id = self
            .brain
            .ingest_event(
                &space_id,
                content.to_string(),
                SourceRef::Note { path: path.clone() },
                trust,
                Some(&attachment),
                "mcp-remember",
            )
            .await
            .map_err(ToolErr::run)?;
        // remember always extracts synchronously (DESIGN §12.2).
        self.try_sample_extract(&space_id, &id, session).await
    }
```

**Required imports:** `SourceRef` and `TrustTier` are re-exported by the `oxibrain` facade crate. Extend the existing `use oxibrain::{…}` block at the top of server.rs by adding `SourceRef, TrustTier,` to the import list:
```rust
use oxibrain::{
    Brain, BrainConfig, BrainError, BriefTarget, Capability, DeclObject, Declaration, EntityRef,
    RedactTarget, Scope, SourceRef, Timestamp, TrustTier,
};
```

`Timestamp` is needed because `build_attachment` names the type in its signature. It is re-exported by the `oxibrain` facade (lib.rs line 30-32: `pub use oxibrain_ports::{…, Timestamp, …}`).

`SourceRef` and `TrustTier` are likewise re-exported by the facade. No `oxibrain_core` or `oxibrain_ports` imports are needed beyond `oxibrain_core::content_hash` (used inline with full path in `build_attachment`).

- [ ] **Step 6: Write server tests**

In `crates/oxibrain-mcp/src/server.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
    #[tokio::test]
    async fn trust_gate_rejects_trusted_without_capability() {
        let (_dir, server) = fresh_scoped(&[Capability::Ingest], &["personal"]).await;
        let msg = msg(1, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "test",
                "space": "personal",
                "trust": "trusted"
            }
        })));
        let resp = server.handle(msg).await.unwrap();
        let err = resp.get("error").expect("must be an error");
        assert_eq!(err["code"], UNAUTHORIZED);
    }

    #[tokio::test]
    async fn trust_gate_allows_trusted_with_capability() {
        let (_dir, server) = fresh_scoped(
            &[Capability::Ingest, Capability::TrustedIngest],
            &["personal"],
        ).await;
        let msg = msg(1, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "test",
                "space": "personal",
                "trust": "trusted"
            }
        })));
        let resp = server.handle(msg).await.unwrap();
        // Should succeed (no error field) or return a tool result.
        assert!(resp.get("error").is_none(), "trusted_ingest cap must allow trust=trusted");
    }

    #[tokio::test]
    async fn trust_gate_allows_ingest_without_trust_param() {
        let (_dir, server) = fresh_scoped(&[Capability::Ingest], &["personal"]).await;
        let msg = msg(1, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "test",
                "space": "personal"
            }
        })));
        let resp = server.handle(msg).await.unwrap();
        assert!(resp.get("error").is_none(), "ingest without trust param must succeed");
    }

    /// Event-path wiring: ingest creates an episode with attachment.
    /// Verifies source_id, occurrence_id, and trust are persisted.
    #[tokio::test]
    async fn ingest_creates_episode_with_attachment() {
        let (_dir, server) = fresh_scoped(&[Capability::Ingest], &["personal"]).await;
        let msg = msg(1, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "attachment test",
                "space": "personal",
                "source_path": "test-source"
            }
        })));
        let resp = server.handle(msg).await.unwrap();
        assert!(resp.get("error").is_none());

        // Extract episode id from the result text.
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let ep_id = text.strip_prefix("Ingested as episode: ").unwrap().to_string();

        // Verify the episode via direct brain access (same-module private field).
        let ep = server.brain.get_episode(&ep_id).await.unwrap().unwrap();
        assert_eq!(ep.content, "attachment test");
        assert_eq!(ep.trust, TrustTier::Trusted, "default trust without param must be Trusted (note-path parity)");

        // Re-push same content: must return the same episode id (idempotent).
        let msg2 = msg(2, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "attachment test",
                "space": "personal",
                "source_path": "test-source"
            }
        })));
        let resp2 = server.handle(msg2).await.unwrap();
        let text2 = resp2["result"]["content"][0]["text"].as_str().unwrap();
        let ep_id2 = text2.strip_prefix("Ingested as episode: ").unwrap().to_string();
        assert_eq!(ep_id, ep_id2, "same content re-push must be idempotent");
    }

    /// Different content at same source_path creates a new episode.
    #[tokio::test]
    async fn ingest_different_content_creates_new_episode() {
        let (_dir, server) = fresh_scoped(&[Capability::Ingest], &["personal"]).await;
        let msg1 = msg(1, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "version 1",
                "space": "personal",
                "source_path": "doc.md"
            }
        })));
        let resp1 = server.handle(msg1).await.unwrap();
        let text1 = resp1["result"]["content"][0]["text"].as_str().unwrap();
        let id1 = text1.strip_prefix("Ingested as episode: ").unwrap().to_string();

        let msg2 = msg(2, "tools/call", Some(json!({
            "name": "ingest",
            "arguments": {
                "content": "version 2",
                "space": "personal",
                "source_path": "doc.md"
            }
        })));
        let resp2 = server.handle(msg2).await.unwrap();
        let text2 = resp2["result"]["content"][0]["text"].as_str().unwrap();
        let id2 = text2.strip_prefix("Ingested as episode: ").unwrap().to_string();
        assert_ne!(id1, id2, "different content at same source must create new episode");
    }
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p oxibrain-mcp && cargo test -p oxibrain-core && cargo test -p oxibrain-cli && cargo test -p oxibrain-client`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/oxibrain-core/src/security.rs crates/oxibrain-mcp/src/server.rs \
        crates/oxibrain-cli/src/cmd/token.rs crates/oxibrain-client/tests/foundation_packages.rs
git commit -m "feat(mcp): trust gate, event-path wiring, TrustedIngest capability, Scope.label"
```

---

### Task 8: E2E — trust gate through daemon socket

**Files:**
- Modify: `crates/oxibrain-cli/tests/serve.rs`

**Interfaces:**
- Consumes: Tasks 1–7.
- Produces: e2e proof that a token without TrustedIngest cannot claim trust=trusted through the daemon socket.

**Critical constraint:** `Brain::open` acquires an advisory lock. The daemon holds this lock while running. Therefore, the token must be issued BEFORE the daemon starts: open Brain, issue token, drop Brain, then spawn daemon.

- [ ] **Step 1: Write the test**

Append to `crates/oxibrain-cli/tests/serve.rs`:

```rust
#[tokio::test]
async fn trust_gate_enforced_through_daemon_socket() {
    use oxibrain::{Brain, BrainConfig, Capability, Scope};
    use oxibrain_client::BrainClient;

    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock_path = dir.path().join("test.sock");

    // Pre-issue a token BEFORE the daemon starts (advisory lock conflict).
    let secret = {
        let brain = Brain::open(BrainConfig::at(&data_dir)).await.unwrap();
        let space_id = brain.ensure_space("personal").await.unwrap();
        let scope = Scope {
            spaces: vec![space_id],
            caps: [Capability::Ingest].into_iter().collect(),
            predicate_filter: None,
            entity_type_filter: None,
            expires_at: None,
            label: String::new(),
        };
        let (_info, secret) = brain.issue_token(&scope, "test", None).await.unwrap();
        drop(brain); // Release advisory lock before daemon starts.
        secret
    };

    // Spawn daemon with --require-token.
    let mut child = spawn_daemon(
        &data_dir,
        &["--daemon", "--socket", sock_path.to_str().unwrap(), "--require-token"],
        None,
    );

    let appeared = wait_for_socket(&sock_path, Duration::from_secs(5)).await;
    assert!(appeared, "daemon must start");

    // Connect with token and try trust=trusted.
    let mut client = BrainClient::connect_with_token(&sock_path, &secret)
        .await
        .expect("connect with token");

    let result = client
        .call_tool(
            "ingest",
            serde_json::json!({
                "content": "test content",
                "space": "personal",
                "trust": "trusted"
            }),
        )
        .await;

    // Must fail: token lacks TrustedIngest capability.
    assert!(result.is_err(), "trust=trusted without TrustedIngest must be rejected");

    // Without trust param, ingest must succeed.
    let result = client
        .call_tool(
            "ingest",
            serde_json::json!({
                "content": "test content without trust",
                "space": "personal"
            }),
        )
        .await;
    assert!(result.is_ok(), "ingest without trust param must succeed");

    let _ = child.kill();
    let _ = child.wait();
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p oxibrain-cli --test serve trust_gate_enforced`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/oxibrain-cli/tests/serve.rs
git commit -m "test(cli): e2e trust gate enforcement through daemon socket"
```

---

### Task 9: Documentation and canon update

**Files:**
- Modify: `doc/ARCHITECTURE.md`
- Modify: `AGENTS.md` (if architecture invariants changed)

**Interfaces:**
- Consumes: All previous tasks.
- Produces: Updated architecture doc reflecting event identity and server-evaluated trust.

- [ ] **Step 1: Update ARCHITECTURE.md**

Update the version header (bump date, increment version). Add or update sections covering:

1. Event identity: `(space_id, source_id, occurrence_id)` is the episode identity for new-path episodes. `content_hash` is integrity verification, not identity.
2. Server-evaluated trust: clients cannot assign `TrustTier`; effective trust comes from source policy declarations.
3. The `TrustedIngest` capability as the sole exception.
4. Schema v10: episodes table no longer has `UNIQUE(space_id, content_hash)`.

- [ ] **Step 2: Verify full workspace**

Run: `cargo build && cargo test && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: all PASS.

- [ ] **Step 3: Commit**

```bash
git add doc/ARCHITECTURE.md AGENTS.md
git commit -m "docs: event identity and server-evaluated trust in architecture canon"
```

---

## Deferred to later plans

- Vault watcher (P5 spec §5) — separate plan; it consumes `occurrence_id` derivation from Task 1 unchanged.
- Pull connector cursor advancement — separate plan; uses `occurrence_id` with predecessor chaining.
- Bootstrap policy declarations for default source kinds (spec §5.3) — separate plan; requires the kernel control source concept.
- Redaction of event-identity episodes — existing redaction path works unchanged since it operates on episode id.
