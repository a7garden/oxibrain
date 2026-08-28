# Space Lifecycle & Default Configuration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make spaces a user-managed unit — explicit `space add/remove/default`, a toml-configured default space, per-space vault provisioning under `~/.oxi/vault/<space>/` — per spec `docs/superpowers/specs/2026-08-27-space-lifecycle-design.md`.

**Architecture:** No new concept above spaces. A `UserConfig` toml module in the facade resolves the default space everywhere (CLI flag > `~/.oxi/config.toml` > `"personal"`). Creation moves to `init` / `space add` / import paths only; every other surface resolves via read-only lookup. Removal is P5-compliant: empty spaces drop directly, `--purge` runs an audited space-scoped redaction plus a documents-cache sweep, never touching vault files.

**Tech Stack:** Rust 2024, clap, rusqlite (bundled), toml/toml_edit, tokio. Spec: §-references below point at the design spec.

## Global Constraints

- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --all`.
- Production code: no `.unwrap()` outside tests (`#![cfg_attr(test, allow(clippy::unwrap_used))]` style).
- English comments/doc-comments/commit messages; conventional commits (`feat:`, `test:`, `docs:`).
- No dependency on `oxios-*`/`oxicode-*`; no new MCP tools (fifteen-tool cap untouched).
- P5: destruction only through audited redaction. P8: one writer per store, scoped to one operation. P9: store fetches, core decides, facade sequences.
- Invariants from spec §4: name rule = trimmed, 1–64 chars, Unicode letters/digits/`-`/`_` only. Default order: `--space` > config.toml > `"personal"`. Vault provisioning only when resolved brain dir == `$HOME/.oxi/brain`.
- Workspace version stays `0.8.0` until Task 12 bumps it to `0.9.0`.

---

### Task 1: Typed space errors + name validation

**Files:**
- Modify: `crates/oxibrain-ports/src/error.rs`
- Create: `crates/oxibrain-core/src/spaces.rs`
- Modify: `crates/oxibrain-core/src/lib.rs` (module index)

**Interfaces:**
- Produces: `BrainError::SpaceNotFound { name: String }`, `BrainError::SpaceRemoveRefused { name: String, reasons: Vec<String> }`, `BrainError::SpaceNameInvalid { name: String, reason: String }`; `oxibrain_core::spaces::validate_space_name(&str) -> Result<(), BrainError>`.

- [ ] **Step 1: Write failing tests for `validate_space_name`**

In `crates/oxibrain-core/src/spaces.rs` (new file, tests included):

```rust
//! Space identity rules. Pure decisions only (P9).

use oxibrain_ports::BrainError;

/// A space name is an identifier: it becomes a directory name under
/// `~/.oxi/vault/` and a token in config. Letters (any script), digits,
/// `-`, `_`; length 1..=64 after trimming. Spec §4.2.
pub fn validate_space_name(raw: &str) -> Result<String, BrainError> {
    let name = raw.trim();
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() {
        return Err(BrainError::SpaceNameInvalid {
            name: raw.to_string(),
            reason: "empty after trimming".into(),
        });
    }
    if chars.len() > 64 {
        return Err(BrainError::SpaceNameInvalid {
            name: raw.to_string(),
            reason: format!("{} chars (max 64)", chars.len()),
        });
    }
    if let Some(bad) = chars.iter().find(|c| {
        !c.is_alphanumeric() && **c != '-' && **c != '_'
    }) {
        return Err(BrainError::SpaceNameInvalid {
            name: raw.to_string(),
            reason: format!("disallowed character {bad:?} (letters, digits, '-', '_')"),
        });
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_slug_and_unicode() {
        assert_eq!(validate_space_name("personal").unwrap(), "personal");
        assert_eq!(validate_space_name(" dev-2 ").unwrap(), "dev-2");
        assert_eq!(validate_space_name("개인").unwrap(), "개인");
    }

    #[test]
    fn rejects_bad_names() {
        assert!(validate_space_name("").is_err());
        assert!(validate_space_name("   ").is_err());
        assert!(validate_space_name("has space").is_err());
        assert!(validate_space_name("a/b").is_err());
        assert!(validate_space_name("a\\b").is_err());
        assert!(validate_space_name("a:b").is_err());
        assert!(validate_space_name(&"x".repeat(65)).is_err());
    }
}
```

- [ ] **Step 2: Register module, add error variants, run tests**

`crates/oxibrain-core/src/lib.rs`: add `pub mod spaces;` (module-per-file index).

`crates/oxibrain-ports/src/error.rs` — append variants before the closing brace (after `Busy`):

```rust
    #[error("space '{name}' not found — create it with: oxibrain space add {name}")]
    SpaceNotFound { name: String },
    #[error("space '{name}' cannot be removed: {reasons:?}")]
    SpaceRemoveRefused { name: String, reasons: Vec<String> },
    #[error("invalid space name '{name}': {reason}")]
    SpaceNameInvalid { name: String, reason: String },
```

Run: `cargo test -p oxibrain-core spaces && cargo test -p oxibrain-ports`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/oxibrain-ports/src/error.rs crates/oxibrain-core/src/spaces.rs crates/oxibrain-core/src/lib.rs
git commit -m "feat: typed space errors and space name validation"
```

---

### Task 2: `UserConfig` — `~/.oxi/config.toml`

**Files:**
- Create: `crates/oxibrain/src/config.rs`
- Modify: `crates/oxibrain/src/lib.rs` (export), `crates/oxibrain/Cargo.toml` (add `toml_edit = "0.8"`)

**Interfaces:**
- Produces: `UserConfig { default_space: String }`; `UserConfig::load(home: Option<&Path>) -> Result<UserConfig, BrainError>`; `UserConfig::resolve_space(flag: Option<&str>, home: Option<&Path>) -> Result<String, BrainError>`; `UserConfig::set_default_space(home: &Path, name: &str) -> Result<(), BrainError>`. Config path: `home/.oxi/config.toml`. Malformed file ⇒ `BrainError::Config` naming the path; unknown keys ignored; missing file ⇒ `default_space = "personal"`.

- [ ] **Step 1: Write failing tests**

```rust
//! `~/.oxi/config.toml` — the §18 user config. First key: `default_space`.
//!
//! Strict parse (a malformed file fails every command rather than silently
//! changing where data lands); unknown keys ignored (forward compat);
//! `set_default_space` round-trips through `toml_edit` so user comments and
//! reserved keys survive.

use oxibrain_ports::BrainError;
use std::path::{Path, PathBuf};

pub const BUILTIN_DEFAULT_SPACE: &str = "personal";
const CONFIG_RELPATH: [&str; 2] = [".oxi", "config.toml"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserConfig {
    pub default_space: String,
}

#[derive(serde::Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    default_space: Option<String>,
}

impl UserConfig {
    pub fn config_path(home: &Path) -> PathBuf {
        home.join(CONFIG_RELPATH[0]).join(CONFIG_RELPATH[1])
    }

    /// Load `~/.oxi/config.toml`. Missing file/`HOME` ⇒ built-in default.
    pub fn load(home: Option<&Path>) -> Result<Self, BrainError> {
        let Some(home) = home else {
            return Ok(Self::builtin());
        };
        let path = Self::config_path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::builtin());
            }
            Err(e) => {
                return Err(BrainError::Config(format!("{}: {e}", path.display())));
            }
        };
        let raw: RawConfig = toml::from_str(&text).map_err(|e| {
            BrainError::Config(format!("{}: parse error: {e}", path.display()))
        })?;
        Ok(Self {
            default_space: raw
                .default_space
                .unwrap_or_else(|| BUILTIN_DEFAULT_SPACE.into()),
        })
    }

    pub fn builtin() -> Self {
        Self {
            default_space: BUILTIN_DEFAULT_SPACE.into(),
        }
    }

    /// Spec §4.1 resolution: `--space` flag > config.toml > built-in.
    pub fn resolve_space(
        flag: Option<&str>,
        home: Option<&Path>,
    ) -> Result<String, BrainError> {
        if let Some(f) = flag {
            return Ok(f.trim().to_string());
        }
        Ok(Self::load(home)?.default_space)
    }

    /// Write `default_space`, preserving comments and unknown keys.
    pub fn set_default_space(home: &Path, name: &str) -> Result<(), BrainError> {
        let path = Self::config_path(home);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| BrainError::Config(format!("{}: {e}", path.display())))?;
        doc["default_space"] = toml_edit::value(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| BrainError::Config(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(&path, doc.to_string())
            .map_err(|e| BrainError::Config(format!("{}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_and_home_fall_back_to_builtin() {
        assert_eq!(UserConfig::load(None).unwrap().default_space, "personal");
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            UserConfig::load(Some(tmp.path())).unwrap().default_space,
            "personal"
        );
    }

    #[test]
    fn loads_default_space_and_ignores_unknown_keys() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            UserConfig::config_path(tmp.path()),
            "# comment\ndefault_space = \"dev\"\nprovider = \"local\"\n",
        )
        .unwrap();
        assert_eq!(UserConfig::load(Some(tmp.path())).unwrap().default_space, "dev");
    }

    #[test]
    fn malformed_file_is_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UserConfig::config_path(tmp.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "not [ valid toml").unwrap();
        let err = UserConfig::load(Some(tmp.path())).unwrap_err();
        assert!(matches!(err, BrainError::Config(_)));
        assert!(err.to_string().contains("config.toml"));
    }

    #[test]
    fn resolution_order_flag_beats_file_beats_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".oxi")).unwrap();
        std::fs::write(
            UserConfig::config_path(tmp.path()),
            "default_space = \"dev\"\n",
        )
        .unwrap();
        let home = Some(tmp.path());
        assert_eq!(UserConfig::resolve_space(Some("work"), home).unwrap(), "work");
        assert_eq!(UserConfig::resolve_space(None, home).unwrap(), "dev");
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            UserConfig::resolve_space(None, Some(empty.path())).unwrap(),
            "personal"
        );
    }

    #[test]
    fn set_default_space_preserves_comments_and_unknown_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UserConfig::config_path(tmp.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "# my config\nprovider = \"local\"\n").unwrap();
        UserConfig::set_default_space(tmp.path(), "dev").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("provider = \"local\""));
        assert!(text.contains("default_space = \"dev\""));
        assert_eq!(UserConfig::load(Some(tmp.path())).unwrap().default_space, "dev");
    }
}
```

- [ ] **Step 2: Register and run**

`crates/oxibrain/src/lib.rs`: `pub mod config;`. `crates/oxibrain/Cargo.toml` `[dependencies]`: add `toml_edit = "0.8"` (toml 0.8 already present; same version family, no new lock entries beyond toml_edit's own crate).

Run: `cargo test -p oxibrain config`
Expected: PASS (5 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/oxibrain/src/config.rs crates/oxibrain/src/lib.rs crates/oxibrain/Cargo.toml Cargo.lock
git commit -m "feat: user config toml with default_space resolution"
```

---

### Task 3: Store primitives — counts, drop, documents purge

**Files:**
- Modify: `crates/oxibrain-store/src/ledger.rs`
- Modify: `crates/oxibrain-store/src/documents.rs`
- Modify: `crates/oxibrain/src/lib.rs` (facade wrappers)

**Interfaces:**
- Produces (store): `ledger::episode_count_for_space(conn, space_id) -> Result<i64, BrainError>`; `ledger::drop_space(conn, space_id) -> Result<(), BrainError>` (bare `DELETE FROM spaces`; callers verify emptiness first); `DocumentCache::document_count_for_space(&self, space) -> Result<u64, BrainError>`; `DocumentCache::chunk_count_for_space(&self, space) -> Result<u64, BrainError>`; `DocumentCache::purge_space(&self, space) -> Result<(), BrainError>`.
- Produces (facade): `Brain::episode_count_for_space(&self, space_id) -> Result<i64, BrainError>`; `Brain::document_count_for_space(&self, space_name) -> Result<u64, BrainError>`; `Brain::chunk_count_for_space(&self, space_name) -> Result<u64, BrainError>`; `Brain::purge_documents_for_space(&self, space_name) -> Result<(), BrainError>`.

- [ ] **Step 1: Failing store tests (documents cache)**

In `crates/oxibrain-store/src/documents.rs` tests module (pattern: existing `apply_plan`/`upsert_document` helpers around line 1099–1160):

```rust
#[test]
fn document_and_chunk_counts_are_space_scoped() {
    let dir = tempfile::tempdir().unwrap();
    {
        let cache = DocumentCache::open_rw(dir.path()).unwrap();
        let plan = add_plan("alpha", "s1", "a.md", "r1");
        cache.apply(&plan).unwrap();
        let plan2 = add_plan("beta", "s2", "b.md", "r1");
        cache.apply(&plan2).unwrap();
    }
    let cache = DocumentCache::open_ro(dir.path()).unwrap();
    assert_eq!(cache.document_count_for_space("s1").unwrap(), 1);
    assert_eq!(cache.chunk_count_for_space("s1").unwrap() > 0, true);
    assert_eq!(cache.document_count_for_space("s2").unwrap(), 1);
}

#[test]
fn purge_space_removes_only_that_space() {
    let dir = tempfile::tempdir().unwrap();
    {
        let cache = DocumentCache::open_rw(dir.path()).unwrap();
        cache.apply(&add_plan("alpha", "s1", "a.md", "r1")).unwrap();
        cache.apply(&add_plan("beta", "s2", "b.md", "r1")).unwrap();
        cache.purge_space("s1").unwrap();
    }
    let cache = DocumentCache::open_ro(dir.path()).unwrap();
    assert_eq!(cache.document_count_for_space("s1").unwrap(), 0);
    assert_eq!(cache.chunk_count_for_space("s1").unwrap(), 0);
    assert_eq!(cache.document_count_for_space("s2").unwrap(), 1);
}
```

- [ ] **Step 2: Implement store fns**

`ledger.rs` (beside `list_spaces`):

```rust
/// Episode count for one space (decision-free fetch, P9).
pub fn episode_count_for_space(conn: &Connection, space_id: &str) -> Result<i64, BrainError> {
    conn.query_row(
        "SELECT COUNT(*) FROM episodes WHERE space_id = ?1",
        params![space_id],
        |r| r.get(0),
    )
    .map_err(sql_err)
}

/// Drop a space row. Callers MUST have verified the space is empty or have
/// purged its data (spec §4.5) — this deletes no dependent rows.
pub fn drop_space(conn: &Connection, space_id: &str) -> Result<(), BrainError> {
    conn.execute("DELETE FROM spaces WHERE id = ?1", params![space_id])
        .map_err(sql_err)?;
    Ok(())
}
```

`documents.rs` (impl DocumentCache, beside `embedded_count`):

```rust
/// Rows in `documents` for one space (listing stat; spec §4.2).
pub fn document_count_for_space(&self, space: &str) -> Result<u64, BrainError> {
    let n: i64 = self
        .conn
        .query_row(
            "SELECT COUNT(*) FROM documents WHERE space = ?1",
            params![space],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    Ok(n as u64)
}

/// Chunks for one space (empty-check input; spec §4.5).
pub fn chunk_count_for_space(&self, space: &str) -> Result<u64, BrainError> {
    let n: i64 = self
        .conn
        .query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE space = ?1",
            params![space],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    Ok(n as u64)
}

/// Delete every row this cache holds for a space (documents.db is a
/// disposable cache — no episode semantics involved). Spec §4.5 step 3.
pub fn purge_space(&self, space: &str) -> Result<(), BrainError> {
    for sql in [
        "DELETE FROM doc_fts_word WHERE space = ?1",
        "DELETE FROM doc_fts_ngram WHERE space = ?1",
        "DELETE FROM doc_vectors WHERE chunk_id IN (SELECT id FROM doc_chunks WHERE space = ?1)",
        "DELETE FROM doc_chunks WHERE space = ?1",
        "DELETE FROM doc_manifest WHERE root_alias IN (SELECT alias FROM doc_roots WHERE space = ?1)",
        "DELETE FROM documents WHERE space = ?1",
        "DELETE FROM doc_roots WHERE space = ?1",
    ] {
        self.conn.execute(sql, params![space]).map_err(sql_err)?;
    }
    Ok(())
}
```

Run: `cargo test -p oxibrain-store documents`
Expected: PASS.

- [ ] **Step 3: Facade wrappers**

In `crates/oxibrain/src/lib.rs` (Brain impl, beside `list_spaces`; follow the file's existing `self.read` / `spawn_blocking` / `DocumentCache::open_ro` patterns):

```rust
/// Episode count for one space id (space-remove empty check, spec §4.5).
pub async fn episode_count_for_space(&self, space_id: &str) -> Result<i64, BrainError> {
    let space_id = space_id.to_string();
    self.read(move |conn| ledger::episode_count_for_space(conn, &space_id))
        .await
}

/// Document rows in the documents cache for a space NAME (listing, spec §4.2).
pub async fn document_count_for_space(&self, space_name: &str) -> Result<u64, BrainError> {
    let dir = self.config.dir.clone();
    let name = space_name.to_string();
    tokio::task::spawn_blocking(move || match DocumentCache::open_ro(&dir) {
        Ok(cache) => cache.document_count_for_space(&name),
        Err(BrainError::NotFound(_)) => Ok(0),
        Err(e) => Err(e),
    })
    .await
    .map_err(|e| BrainError::Storage(format!("join: {e}")))?
}

/// Chunk count for a space NAME (empty check, spec §4.5).
pub async fn chunk_count_for_space(&self, space_name: &str) -> Result<u64, BrainError> {
    let dir = self.config.dir.clone();
    let name = space_name.to_string();
    tokio::task::spawn_blocking(move || match DocumentCache::open_ro(&dir) {
        Ok(cache) => cache.chunk_count_for_space(&name),
        Err(BrainError::NotFound(_)) => Ok(0),
        Err(e) => Err(e),
    })
    .await
    .map_err(|e| BrainError::Storage(format!("join: {e}")))?
}

/// Purge every documents-cache row for a space NAME (purge step 3).
pub async fn purge_documents_for_space(&self, space_name: &str) -> Result<(), BrainError> {
    let dir = self.config.dir.clone();
    let name = space_name.to_string();
    tokio::task::spawn_blocking(move || {
        let cache = DocumentCache::open_rw(&dir)?;
        cache.purge_space(&name)
    })
    .await
    .map_err(|e| BrainError::Storage(format!("join: {e}")))?
}
```

(`DocumentCache` import already exists in the facade via `oxibrain_store::documents`; if `lib.rs` does not import it, add `use oxibrain_store::documents::DocumentCache;` — check the existing imports at the top of `lib.rs` / `document_plane.rs` and reuse the same path.)

Run: `cargo test -p oxibrain && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/oxibrain-store/src/ledger.rs crates/oxibrain-store/src/documents.rs crates/oxibrain/src/lib.rs
git commit -m "feat: space-scoped counts, drop_space, and documents cache purge"
```

---

### Task 4: `RedactTarget::Space` — audited space purge in brain.db

**Files:**
- Modify: `crates/oxibrain-core/src/security.rs` (variant)
- Modify: `crates/oxibrain-store/src/redaction.rs` (closure + execute arm)

**Interfaces:**
- Produces: serde variant `{"kind":"space","id":"<space-id>"}`; `redaction::resolve_space_closure(conn, space_id) -> Result<RedactionClosure, BrainError>`; behavior — `resolve_closure`/`execute_redaction` accept `RedactTarget::Space { id }`; after execution NO row in brain.db references the space (except `audit_log`/`redactions` tombstones/`tokens`), and one audit entry with actor/reason exists. Reuses existing `RedactionClosure`/`RedactionResult`.

- [ ] **Step 1: Add the variant (core)**

`crates/oxibrain-core/src/security.rs`, append to `RedactTarget`:

```rust
    /// Redact an entire space and everything derived from it (spec §4.5).
    /// Terminal: no episodes, entities, statements, or beliefs remain.
    Space { id: String },
```

- [ ] **Step 2: Failing tests (store redaction)**

In `redaction.rs` tests (reuse `fresh_db()` / `declare_alice_works_for_acme`):

```rust
#[test]
fn redact_space_removes_everything_and_audits() {
    let conn = fresh_db();
    let now = Timestamp::from_millis(1000);
    let space_id = space_id_of(&conn, "sp");
    declare_alice_works_for_acme(&conn, now);

    let target = RedactTarget::Space { id: space_id.clone() };
    let closure = resolve_closure(&conn, &target).unwrap();
    assert!(!closure.episodes.is_empty());
    assert!(!closure.assertions.is_empty());

    let result = execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
    assert!(!result.closure.episodes.is_empty());

    // Nothing references the space anymore.
    for (table, col) in [
        ("episodes", "space_id"),
        ("entities", "space_id"),
        ("entity_keys", "space_id"),
        ("statements", "space_id"),
        ("communities", "space_id"),
        ("chunks", "space_id"),
    ] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1"),
                [&space_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{table} still has rows for the space");
    }
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM spaces WHERE id = ?1", [&space_id], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "space row must be dropped");
    // Audit trail kept.
    let audited: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit_log WHERE operation = 'redact'", [], |r| r.get(0))
        .unwrap();
    assert!(audited >= 1);
}

#[test]
fn redact_space_is_idempotent() {
    let conn = fresh_db();
    let now = Timestamp::from_millis(1000);
    declare_alice_works_for_acme(&conn, now);
    let space_id = space_id_of(&conn, "sp");
    let target = RedactTarget::Space { id: space_id };
    execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
    let second = execute_redaction(&conn, &target, "purge", "cli", now).unwrap();
    assert!(second.closure.episodes.is_empty());
}
```

Add the helper in the tests module (id derivation mirrors `ledger::space_id`; if `space_id` is private, expose `pub fn space_id(name: &str) -> String` from `ledger.rs` and use it here — preferred, one derivation):

```rust
fn space_id_of(_conn: &Connection, _name: &str) -> String {
    // fresh_db() ensures a default space; resolve its real id:
    conn_space_id(_conn)
}

fn conn_space_id(conn: &Connection) -> String {
    conn.query_row("SELECT id FROM spaces LIMIT 1", [], |r| r.get(0)).unwrap()
}
```

NOTE for implementer: `fresh_db()` seeds one space via `create_space(conn, "sp", now)` or similar (see its body around line 571–578); use the id it actually seeds. Make `ledger::space_id` `pub` in this task and call it directly: `ledger::space_id("sp")` — deterministic blake3, no query needed.

- [ ] **Step 3: Implement**

In `redaction.rs`:

```rust
/// Space-level closure (spec §4.5): every episode in the space and every
/// derived row. Enumerated, not folded per-episode — a space purge leaves
/// nothing to refold.
fn resolve_space_closure(conn: &Connection, space_id: &str) -> Result<RedactionClosure, BrainError> {
    let episodes = query_strings(
        conn,
        "SELECT id FROM episodes WHERE space_id = ?1",
        &[&space_id],
    )?;
    let assertions = query_strings(
        conn,
        "SELECT a.id FROM assertions a
         JOIN statements s ON s.id = a.statement_id
         WHERE s.space_id = ?1",
        &[&space_id],
    )?;
    let statements = query_strings(
        conn,
        "SELECT id FROM statements WHERE space_id = ?1",
        &[&space_id],
    )?;
    let mentions = query_strings(
        conn,
        "SELECT m.id FROM mentions m
         JOIN assertions a ON a.id = m.assertion_id
         JOIN statements s ON s.id = a.statement_id
         WHERE s.space_id = ?1",
        &[&space_id],
    )?;
    let extractions = if episodes.is_empty() {
        Vec::new()
    } else {
        query_strings_in(
            conn,
            "SELECT DISTINCT episode_id FROM extractions WHERE ",
            &episodes,
        )?
    };
    let summaries = Vec::new(); // keyed by member-set hash, not space; cache-zone, left to rebuild
    Ok(RedactionClosure { episodes, assertions, statements, mentions, extractions, summaries })
}
```

Wire the arms — `resolve_closure`:

```rust
        RedactTarget::Space { id } => resolve_space_closure(conn, id),
```

`execute_redaction`: add the space arm. It does NOT go through the generic delete path — it is a full space teardown in FK-safe order, then one audit row, matching what `reproject` would produce (nothing) without recomputing. Inside `execute_redaction`, before the existing match, or as an arm that returns early after doing:

```rust
/// Full space teardown (FK-safe order). Mirrors reproject's end state for a
/// space with no episodes: nothing. Tombstones and audit rows stay.
fn execute_space_redaction(
    conn: &Connection,
    space_id: &str,
    reason: &str,
    actor: &str,
    now: Timestamp,
) -> Result<RedactionResult, BrainError> {
    let closure = resolve_space_closure(conn, space_id)?;
    // Ranking half first (no FKs into truth tables).
    for sql in [
        "DELETE FROM fts_word WHERE space = ?1",
        "DELETE FROM fts_ngram WHERE space = ?1",
        "DELETE FROM episodes_fts WHERE space_id = ?1",
        "DELETE FROM tfidf_vectors WHERE space_id = ?1",
        "DELETE FROM entity_vectors WHERE entity_id IN (SELECT id FROM entities WHERE space_id = ?1)",
        "DELETE FROM communities WHERE space_id = ?1",
        "DELETE FROM chunks WHERE space_id = ?1",
    ] {
        conn.execute(sql, params![space_id]).map_err(sql_err)?;
    }
    // Truth half, children first.
    for sql in [
        "DELETE FROM extraction_failures WHERE episode_id IN (SELECT id FROM episodes WHERE space_id = ?1)",
        "DELETE FROM extractions WHERE episode_id IN (SELECT id FROM episodes WHERE space_id = ?1)",
        "DELETE FROM episode_links WHERE from_episode IN (SELECT id FROM episodes WHERE space_id = ?1) OR to_episode IN (SELECT id FROM episodes WHERE space_id = ?1)",
        "DELETE FROM mentions WHERE assertion_id IN (SELECT a.id FROM assertions a JOIN statements s ON s.id = a.statement_id WHERE s.space_id = ?1)",
        "DELETE FROM assertions WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1)",
        "DELETE FROM beliefs WHERE statement_id IN (SELECT id FROM statements WHERE space_id = ?1)",
        "DELETE FROM statements WHERE space_id = ?1",
        "DELETE FROM entity_merges WHERE loser_id IN (SELECT id FROM entities WHERE space_id = ?1) OR winner_id IN (SELECT id FROM entities WHERE space_id = ?1)",
        "DELETE FROM entity_keys WHERE space_id = ?1",
        "DELETE FROM entities WHERE space_id = ?1",
        "DELETE FROM source_policies WHERE source_id IN (SELECT id FROM sources WHERE space_id = ?1)",
        "DELETE FROM sources WHERE space_id = ?1",
        "DELETE FROM episodes WHERE space_id = ?1",
        "DELETE FROM spaces WHERE id = ?1",
    ] {
        conn.execute(sql, params![space_id]).map_err(sql_err)?;
    }
    record_redaction(conn, &RedactTarget::Space { id: space_id.to_string() }, reason, actor, now)?;
    let _ = now;
    Ok(RedactionResult { closure, beliefs_refolded: 0 })
}
```

NOTE for implementer: reuse the existing audit/tombstone recording call inside `execute_redaction` (see how the Episode arm persists the `redactions` row + `audit_log` entry — the function or inline SQL is already there; call the same code). `execute_space_redaction` returns `beliefs_refolded: 0` because nothing remains to fold. If `record_redaction` does not exist as a named fn, inline the same INSERTs the other arms use. Adjust table list ONLY if a listed table does not exist in schema v11 — check `crates/oxibrain-store/src/migrations/*.sql` and keep the list exactly the space-scoped tables.

Run: `cargo test -p oxibrain-store redact`
Expected: PASS (new 2 tests + existing suite).

- [ ] **Step 4: Reprojection determinism guard**

Add to the same tests module:

```rust
#[test]
fn reproject_after_space_redaction_stays_empty() {
    let conn = fresh_db();
    let now = Timestamp::from_millis(1000);
    declare_alice_works_for_acme(&conn, now);
    let space_id = ledger::space_id("sp"); // or the id fresh_db seeds
    execute_redaction(&conn, &RedactTarget::Space { id: space_id }, "t", "t", now).unwrap();
    // Second space with data still reprojects identically (canary).
    let other = create_space(&conn, "other", now).unwrap();
    drop(other);
    oxibrain_store::reproject(&conn).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}
```

If `oxibrain_store::reproject` needs a specific store/conn setup, follow the pattern of the existing test `reproject_after_episode_redaction_preserves_projection` (line ~691).

Run: `cargo test -p oxibrain-store`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-core/src/security.rs crates/oxibrain-store/src/redaction.rs crates/oxibrain-store/src/ledger.rs
git commit -m "feat: RedactTarget::Space — audited full-space redaction"
```

---

### Task 5: CLI plumbing — resolved default space + auto-create abolition

**Files:**
- Modify: `crates/oxibrain-cli/src/cli.rs` (all `space` args → `Option<String>`; new `Space` subcommand enum added in Task 7 — here only the arg-type sweep)
- Modify: `crates/oxibrain-cli/src/main.rs` (resolve helper)
- Modify: `crates/oxibrain-cli/src/cmd/mod.rs` (shared `space_id` helper)
- Modify: every `cmd/*.rs` that calls `ensure_space` EXCEPT `init.rs`, `import_oxios.rs`, `gate.rs`, `eval.rs` (test/import runners keep auto-create)

**Interfaces:**
- Produces: `cmd::resolve_space(flag: Option<&str>, home: Option<&Path>) -> anyhow::Result<String>` (in `main.rs` or `cmd/mod.rs`); `cmd::space_id(brain: &Brain, name: &str) -> anyhow::Result<String>` — read-only lookup, `BrainError::SpaceNotFound` → anyhow error whose message IS the hint (the variant's Display already prints it).

- [ ] **Step 1: Shared helper**

`cmd/mod.rs` append:

```rust
use anyhow::Result;
use oxibrain::Brain;

/// Resolve a space name to its id WITHOUT creating it (spec §4.4).
/// Unknown space ⇒ error carrying the `oxibrain space add` hint.
pub async fn space_id(brain: &Brain, name: &str) -> Result<String> {
    match brain.lookup_space(name).await {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(anyhow::anyhow!(
            "space '{name}' not found — create it with: oxibrain space add {name}"
        )),
        Err(e) => Err(anyhow::anyhow!("lookup space: {e}")),
    }
}
```

- [ ] **Step 2: Sweep the arg types**

In `cli.rs`: every `#[arg(long, default_value = "personal")] space: String` becomes `#[arg(long)] space: Option<String>` (15 occurrences across `Init`, `Ingest`, `Ask`, `Entity*`, `Why`, `Contradictions`, `Page`, `Redact`, `ImportOxios`, `Reextract`, `Predicate::Add`, `Source::Policy`, `Token::Issue`, `Declare`).

In `main.rs`: add

```rust
fn resolve_space(flag: Option<&str>, home: Option<&std::path::Path>) -> anyhow::Result<String> {
    Ok(oxibrain::config::UserConfig::resolve_space(flag, home)?)
}
```

and in each dispatch arm, resolve before calling the cmd (example for `Ask`):

```rust
        Command::Ask { question, space } => {
            let space = resolve_space(space.as_deref(), home.as_deref())?;
            cmd::ask::run(&dir, &question, &space).await
        }
```

Apply the same two-line pattern to every arm that carries a `space` field. `cmd::run` signatures keep `space: &str` — they now receive the resolved name. `Init` resolves the same way.

- [ ] **Step 3: Replace ensure with lookup in verb bodies**

Mechanical, per file — `let space_id = brain.ensure_space(space).await?;` becomes:

```rust
    let space_id = crate::cmd::space_id(&brain, space).await?;
```

Files: `ask.rs`, `contradictions.rs`, `declare.rs`, `entity_alias.rs`, `entity_merge.rs`, `entity_retract.rs`, `entity_show.rs`, `entity_split.rs`, `ingest.rs`, `page.rs`, `predicate.rs`, `redact.rs`, `reextract.rs`, `source_policy.rs`, `timeline.rs`, `token.rs`, `why.rs`. NOT: `init.rs`, `import_oxios.rs`, `gate.rs`, `eval.rs`.

- [ ] **Step 4: Regression test (the shadow-row leak)**

`cmd/ingest.rs` tests (add; pattern: tempdir + Brain::open):

```rust
#[tokio::test]
async fn ingest_unknown_space_creates_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("personal").await.unwrap();
    drop(brain);
    let err = run(dir.path(), "-", "nosuch").await.unwrap_err();
    assert!(err.to_string().contains("space 'nosuch' not found"));
    let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path())).await.unwrap();
    let spaces = brain.list_spaces().await.unwrap();
    assert!(spaces.iter().all(|s| s.name != "nosuch"));
}
```

(If `ingest::run`'s parameter order differs, adapt — current: `run(dir, path, space)`.)

Run: `cargo test -p oxibrain-cli && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS — NOTE: some existing CLI tests may call verbs with spaces they never created; fix those tests by ensuring the space first (`brain.ensure_space(...)` in the test setup), which is the new contract.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-cli/src
git commit -m "feat: resolve default space from config; abolish implicit space creation"
```

---

### Task 6: `spaces` listing — documents column + default marker

**Files:**
- Modify: `crates/oxibrain-cli/src/cmd/spaces.rs`

**Interfaces:**
- Consumes: `Brain::document_count_for_space(space_name)`, `UserConfig::load` (Task 2/3).
- Produces: listing rows `NAME ID CREATED EPISODES ENTITIES DOCS` with `*` prefixed to the default space's name.

- [ ] **Step 1: Failing test**

```rust
#[tokio::test]
async fn spaces_marks_default_and_shows_docs() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("work").await.unwrap();
    drop(brain);
    // No documents.toml roots: DOCS column is 0; default (config absent) is
    // "personal", which is not in the store — no row is marked.
    run(dir.path()).await.unwrap();
}
```

(The table output is printed; the behavioral contract tested here is "does not error when the default space is absent and documents cache is missing".)

- [ ] **Step 2: Implement**

Replace the table block in `run`:

```rust
pub async fn run(dir: &Path, home: Option<&Path>) -> Result<()> {
    let brain = Brain::open_ro(BrainConfig::at(dir)).await?;
    let default_space = oxibrain::config::UserConfig::load(home)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .default_space;
    let spaces = brain.list_spaces().await?;
    println!(
        "{:<26} {:<16} {:<20} {:>9} {:>9} {:>6}",
        "NAME", "ID", "CREATED", "EPISODES", "ENTITIES", "DOCS"
    );
    for s in &spaces {
        let marker = if s.name == default_space { "*" } else { "" };
        let docs = brain.document_count_for_space(&s.name).await.unwrap_or(0);
        let id = s.id.chars().take(16).collect::<String>();
        println!(
            "{:<26} {:<16} {:<20} {:>9} {:>9} {:>6}",
            format!("{}{}", s.name, marker),
            id,
            millis_to_iso(s.created_at.millis()),
            s.episode_count,
            s.entity_count,
            docs
        );
    }
    Ok(())
}
```

Update the `Command::Spaces` arm in `main.rs` to pass `home.as_deref()`.

Run: `cargo test -p oxibrain-cli spaces`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/oxibrain-cli/src/cmd/spaces.rs crates/oxibrain-cli/src/main.rs
git commit -m "feat: spaces listing shows document counts and default marker"
```

---

### Task 7: `space add` + per-space vault provisioning

**Files:**
- Create: `crates/oxibrain-cli/src/cmd/provision.rs`
- Create: `crates/oxibrain-cli/src/cmd/space_add.rs`
- Modify: `crates/oxibrain-cli/src/cmd/mod.rs`, `cli.rs`, `main.rs`

**Interfaces:**
- Consumes: `validate_space_name` (T1), `DocumentsConfig` (`oxibrain_connectors::documents_config::{DocumentsConfig, RootEntry}`; `RootEntry` fields: `alias, path, space, include, exclude, max_file_bytes`), `Brain::ensure_space`.
- Produces: `provision::provision_space_vault(brain_dir: &Path, home: &Path, name: &str) -> Result<ProvisionReport>` where `ProvisionReport { vault_dir: PathBuf, root_added: bool, parent_excludes_added: Vec<String> }`; CLI `oxibrain space add <name>`.

- [ ] **Step 1: Failing tests (provision.rs)**

```rust
use anyhow::Result;
use oxibrain_connectors::documents_config::{DocumentsConfig, RootEntry};
use std::path::{Path, PathBuf};

/// Per-space vault provisioning (spec §4.3). Default brain dir only —
/// callers decide; this fn is the shared body.
pub struct ProvisionReport {
    pub vault_dir: PathBuf,
    pub root_added: bool,
    pub parent_excludes_added: Vec<String>,
}

pub fn provision_space_vault(brain_dir: &Path, home: &Path, name: &str) -> Result<ProvisionReport> {
    let vault_dir = home.join(".oxi").join("vault").join(name);
    std::fs::create_dir_all(&vault_dir)?;

    let mut cfg = DocumentsConfig::load(brain_dir)?;
    let path_str = format("~/.oxi/vault/{name}");

    let mut root_added = false;
    if let Some(existing) = cfg.roots.iter().find(|r| r.alias == name) {
        if existing.path != path_str || existing.space != name {
            anyhow::bail!(
                "root alias '{name}' already exists with path '{}' and space '{}'",
                existing.path, existing.space
            );
        }
    } else {
        cfg.roots.push(RootEntry {
            alias: name.to_string(),
            path: path_str,
            space: name.to_string(),
            include: Default::default(),
            exclude: Default::default(),
            max_file_bytes: Default::default(),
        });
        root_added = true;
    }

    // Parent fixup: a legacy flat root at ~/.oxi/vault would double-index
    // this space's documents into its own space. Exclude the subdir.
    let flat = home.join(".oxi").join("vault");
    let mut parent_excludes_added = Vec::new();
    for root in cfg.roots.iter_mut() {
        if root.path.trim_end_matches('/') == flat.display().to_string()
            || root.path == "~/.oxi/vault" || root.path == "~/.oxi/vault/"
        {
            let pat = format!("{name}/**");
            if !root.exclude.contains(&pat) {
                root.exclude.push(pat.clone());
                parent_excludes_added.push(pat);
            }
        }
    }

    DocumentsConfig::save(brain_dir, &cfg)?;
    Ok(ProvisionReport { vault_dir, root_added, parent_excludes_added })
}
```

NOTE for implementer: check `RootEntry`'s exact field set and whether it derives `Default` per field (documents_config.rs lines 37–70); if `include/exclude/max_file_bytes` are `Option<…>` use `None` (defaults apply at load), else use `DocumentsConfig`'s default helpers (`default_include()` etc. are private — replicate by loading defaults from an empty config: `DocumentsConfig::default()` and cloning its fields). Write tests first to pin the shape:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    #[test]
    fn provisions_dir_root_and_idempotent() {
        let brain = tempfile::tempdir().unwrap();
        let h = home();
        let r1 = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(r1.vault_dir.is_dir());
        assert!(r1.root_added);
        let r2 = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(!r2.root_added); // idempotent
        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        assert_eq!(cfg.roots.iter().filter(|r| r.alias == "dev").count(), 1);
    }

    #[test]
    fn flat_vault_root_gains_exclude() {
        let brain = tempfile::tempdir().unwrap();
        let h = home();
        // Seed a legacy flat root first.
        let mut cfg = DocumentsConfig::default();
        cfg.roots.push(RootEntry {
            alias: "vault".into(),
            path: "~/.oxi/vault".into(),
            space: "personal".into(),
            include: Default::default(),
            exclude: Default::default(),
            max_file_bytes: Default::default(),
        });
        DocumentsConfig::save(brain.path(), &cfg).unwrap();

        let r = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert_eq!(r.parent_excludes_added, vec!["dev/**".to_string()]);
        let cfg = DocumentsConfig::load(brain.path()).unwrap();
        let flat = cfg.roots.iter().find(|r0| r0.alias == "vault").unwrap();
        assert!(flat.exclude.contains(&"dev/**".to_string()));
        // Re-run: no duplicate exclude.
        let r2 = provision_space_vault(brain.path(), h.path(), "dev").unwrap();
        assert!(r2.parent_excludes_added.is_empty());
    }
}
```

- [ ] **Step 2: `space add` command**

`cli.rs` — add subcommand:

```rust
    /// Manage spaces (spec: space lifecycle).
    Space {
        #[command(subcommand)]
        command: SpaceCmd,
    },
```

and (top-level enums beside `EntityCmd`):

```rust
#[derive(clap::Subcommand)]
pub enum SpaceCmd {
    /// Create a space (idempotent) and provision its vault dir + root.
    Add { name: String },
    /// Remove an empty space; --purge redacts everything (audited).
    Remove {
        name: String,
        #[arg(long)]
        purge: bool,
    },
    /// Print or set the default space (~/.oxi/config.toml).
    Default { name: Option<String> },
}
```

`cmd/space_add.rs`:

```rust
//! `oxibrain space add <name>` — explicit creation + provisioning.

use crate::cmd::provision::provision_space_vault;
use oxibrain::{Brain, BrainConfig};
use oxibrain_core::spaces::validate_space_name;
use std::path::Path;

pub async fn run(dir: &Path, explicit_dir: bool, home: Option<&Path>, raw: &str) -> anyhow::Result<()> {
    let name = validate_space_name(raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(&name).await?;
    println!("space '{name}' -> {id}");

    match (home, explicit_dir) {
        (Some(h), false) => {
            let r = provision_space_vault(dir, h, &name)?;
            println!("vault dir: {}", r.vault_dir.display());
            if r.root_added {
                println!("documents root '{name}' added");
            }
            for pat in &r.parent_excludes_added {
                println!("parent flat root: excluded '{pat}'");
            }
        }
        _ => println!(
            "deliberate store (--dir): no vault provisioning — manage documents.toml roots yourself"
        ),
    }
    Ok(())
}
```

`main.rs` arm:

```rust
        Command::Space { command } => match command {
            cli::SpaceCmd::Add { name } => {
                cmd::space_add::run(&dir, explicit_dir, home.as_deref(), &name).await
            }
            cli::SpaceCmd::Remove { name, purge } => {
                cmd::space_remove::run(&dir, home.as_deref(), &name, purge).await
            }
            cli::SpaceCmd::Default { name } => {
                cmd::space_default::run(&dir, home.as_deref(), name.as_deref()).await
            }
        },
```

(Task 8/9 supply `space_remove`/`space_default`; to keep this task compiling, add them as `todo!()`-free minimal stubs is FORBIDDEN — instead land Tasks 7–9 as one commit if preferred, or order: implement all three command files in this task series before running the CLI crate tests. Recommended: implement Task 7 fully including temporary direct `space_remove`/`space_default` calls is NOT needed — commit Tasks 7–9 together if the build must stay green per-commit; otherwise sequence them in one working session.)

Run: `cargo test -p oxibrain-cli provision && cargo test -p oxibrain-cli space`
Expected: PASS (with Tasks 8–9 in place; run the combined suite after Task 9).

- [ ] **Step 3: Commit (with Tasks 8–9 if batched)**

```bash
git add crates/oxibrain-cli/src
git commit -m "feat: space add with per-space vault provisioning"
```

---

### Task 8: `space remove` / `--purge`

**Files:**
- Create: `crates/oxibrain-cli/src/cmd/space_remove.rs`

**Interfaces:**
- Consumes: `cmd::space_id` (T5), `Brain::{episode_count_for_space, chunk_count_for_space, purge_documents_for_space, redact, redact_dry_run, lookup_space}`, `UserConfig::load` (T2), `DocumentsConfig` load/save, `RedactTarget::Space` (T4), `provision_space_vault`'s vault-dir derivation (`home/.oxi/vault/<name>`).
- Produces: `space_remove::run(dir, home, name, purge) -> anyhow::Result<()>`.

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use oxibrain::{Brain, BrainConfig};

    async fn brain(dir: &std::path::Path) -> Brain {
        Brain::open(BrainConfig::at(dir)).await.unwrap()
    }

    #[tokio::test]
    async fn remove_unknown_space_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let err = run(dir.path(), Some(home.path()), "ghost", false).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn remove_default_space_refuses() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join(".oxi")).unwrap();
        std::fs::write(
            home.path().join(".oxi").join("config.toml"),
            "default_space = \"keep\"\n",
        )
        .unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("keep").await.unwrap();
        drop(b);
        let err = run(dir.path(), Some(home.path()), "keep", false).await.unwrap_err();
        assert!(err.to_string().contains("default"));
    }

    #[tokio::test]
    async fn remove_fresh_space_drops_scaffold() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("dev").await.unwrap();
        drop(b);
        // Provisioned scaffold (root alias==dev, path ~/.oxi/vault/dev).
        crate::cmd::provision::provision_space_vault(dir.path(), home.path(), "dev").unwrap();
        run(dir.path(), Some(home.path()), "dev", false).await.unwrap();
        let b = brain(dir.path()).await;
        assert!(b.list_spaces().await.unwrap().iter().all(|s| s.name != "dev"));
    }

    #[tokio::test]
    async fn purge_keeps_vault_files_and_audits() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let sid = b.ensure_space("work").await.unwrap();
        use oxibrain::{SourceRef, TrustTier};
        let _ = b
            .ingest_note(&sid, "test://t", "Alice works for Acme".into(),
                oxibrain_ports::Timestamp::from_millis(1000), SourceRef::Note,
                TrustTier::SemiTrusted)
            .await
            .unwrap();
        drop(b);
        // User file in the vault dir — must survive purge.
        let vault = home.path().join(".oxi").join("vault").join("work");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("note.md"), "user data").unwrap();

        run(dir.path(), Some(home.path()), "work", true).await.unwrap();

        assert_eq!(std::fs::read_to_string(vault.join("note.md")).unwrap(), "user data");
        let b = brain(dir.path()).await;
        assert!(b.list_spaces().await.unwrap().iter().all(|s| s.name != "work"));
        let audit = b.audit_log(Some(10)).await.unwrap();
        assert!(audit.iter().any(|a| a.operation.contains("redact")));
    }
}
```

(Adapt the `ingest_note` call to its real signature — check `cmd/ingest.rs` lines 16–23 for the exact parameters and copy them.)

- [ ] **Step 2: Implement**

```rust
//! `oxibrain space remove <name> [--purge]` — spec §4.5.

use crate::cmd::space_id;
use oxibrain::config::UserConfig;
use oxibrain::{Brain, BrainConfig, RedactTarget};
use oxibrain_connectors::documents_config::DocumentsConfig;
use std::path::Path;

pub async fn run(dir: &Path, home: Option<&Path>, name: &str, purge: bool) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let sid = space_id(&brain, name).await?;

    let default = UserConfig::load(home).map_err(|e| anyhow::anyhow!("{e}"))?.default_space;
    if name == default {
        anyhow::bail!(
            "space '{name}' is the default — change it first: oxibrain space default <other>"
        );
    }

    let mut cfg = DocumentsConfig::load(dir)?;
    // Scaffold detection (Task 7 review finding): documents.toml stores
    // EXPANDED absolute paths — `DocumentsConfig::load` expands `~` against
    // $HOME and `save` serializes the expanded form, so a tilde-literal
    // comparison never matches. The provisioning scaffold is uniquely
    // identified by alias == space == name (aliases are unique in
    // documents.toml); filter on that.
    let roots_referencing: Vec<_> = cfg
        .roots
        .iter()
        .filter(|r| r.space == name && r.alias != name)
        .cloned()
        .collect();

    if purge {
        // 1. documents.toml first — abort before any destruction on failure.
        cfg.roots.retain(|r| r.space != name);
        DocumentsConfig::save(dir, &cfg)?;
        // 2. brain.db: audited full-space redaction (drops the spaces row).
        let target = RedactTarget::Space { id: sid.clone() };
        let result = brain.redact(&target, "space purge", "cli").await?;
        println!(
            "purged: {} episodes, {} assertions, {} statements, {} mentions",
            result.closure.episodes.len(),
            result.closure.assertions.len(),
            result.closure.statements.len(),
            result.closure.mentions.len()
        );
        // 3. documents cache.
        brain.purge_documents_for_space(name).await?;
    } else {
        let episodes = brain.episode_count_for_space(&sid).await?;
        let chunks = brain.chunk_count_for_space(name).await?;
        let mut reasons = Vec::new();
        if episodes > 0 {
            reasons.push(format!("{episodes} episodes (use --purge)"));
        }
        if chunks > 0 {
            reasons.push(format!("{chunks} document chunks (use --purge)"));
        }
        if !roots_referencing.is_empty() {
            reasons.push(format!(
                "{} documents.toml root(s) reference it (use --purge)",
                roots_referencing.len()
            ));
        }
        if !reasons.is_empty() {
            anyhow::bail!("space '{name}' is not empty: {}", reasons.join("; "));
        }
        // Empty removal: scaffold root out first, then the row.
        cfg.roots.retain(|r| r.space != name);
        DocumentsConfig::save(dir, &cfg)?;
        brain.drop_space(&sid).await?;
        brain.purge_documents_for_space(name).await?;
    }

    // 4. Vault dir: never delete files; rmdir only when empty.
    if let Some(h) = home {
        let vault = h.join(".oxi").join("vault").join(name);
        if vault.is_dir() && std::fs::read_dir(&vault)?.next().is_none() {
            std::fs::remove_dir(&vault)?;
        } else if vault.is_dir() {
            println!("vault dir kept (has files): {}", vault.display());
        }
    }
    println!("space '{name}' removed");
    Ok(())
}
```

Add facade passthrough (Task 3 pattern) if missing:

```rust
/// Drop a space row (verified empty by the caller). Spec §4.5.
pub async fn drop_space(&self, space_id: &str) -> Result<(), BrainError> {
    let space_id = space_id.to_string();
    self.write(move |conn| ledger::drop_space(conn, &space_id)).await
}
```

NOTE: check `Brain::redact`'s exact signature (`cmd/redact.rs:19` calls `brain.redact(&parsed, reason, "cli")`) and `ingest_note`'s parameter list before finalizing; copy the real call shapes.

Run: `cargo test -p oxibrain-cli space_remove`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/oxibrain-cli/src crates/oxibrain/src/lib.rs
git commit -m "feat: space remove with empty checks and audited purge"
```

---

### Task 9: `space default`

**Files:**
- Create: `crates/oxibrain-cli/src/cmd/space_default.rs`

**Interfaces:**
- Consumes: `UserConfig::{load, set_default_space}` (T2), `cmd::space_id` (T5).
- Produces: `space_default::run(dir, home, name: Option<&str>)`.

- [ ] **Step 1: Implement with tests**

```rust
//! `oxibrain space default [<name>]` — print or set the default space.

use crate::cmd::space_id;
use oxibrain::config::UserConfig;
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, home: Option<&Path>, name: Option<&str>) -> anyhow::Result<()> {
    let Some(home) = home else {
        anyhow::bail!("$HOME is required to read/write ~/.oxi/config.toml");
    };
    let Some(name) = name else {
        let cfg = UserConfig::load(Some(home)).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("{}", cfg.default_space);
        return Ok(());
    };
    let name = oxibrain_core::spaces::validate_space_name(name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // The default must exist in the resolved brain (spec §4.2).
    let brain = Brain::open_ro(BrainConfig::at(dir)).await?;
    let _ = space_id(&brain, &name).await?;
    UserConfig::set_default_space(home, &name).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("default space: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_and_print_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let _ = b.ensure_space("dev").await.unwrap();
        drop(b);
        run(dir.path(), Some(home.path()), Some("dev")).await.unwrap();
        assert!(UserConfig::load(Some(home.path())).unwrap().default_space == "dev");
        run(dir.path(), Some(home.path()), None).await.unwrap(); // prints "dev"
    }

    #[tokio::test]
    async fn set_unknown_space_fails_without_writing() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        assert!(run(dir.path(), Some(home.path()), Some("ghost")).await.is_err());
        assert!(!UserConfig::config_path(home.path()).exists());
    }
}
```

Run: `cargo test -p oxibrain-cli space_default && cargo test -p oxibrain-cli`
Expected: PASS (full CLI suite — Tasks 7–9 green together).

- [ ] **Step 2: Commit**

```bash
git add crates/oxibrain-cli/src
git commit -m "feat: space default prints and sets the toml-configured default"
```

---

### Task 10: `init` rework

**Files:**
- Modify: `crates/oxibrain-cli/src/cmd/init.rs`

**Interfaces:**
- Consumes: `provision_space_vault` (T7), `UserConfig` (T2), `validate_space_name` (T1).
- Produces: init behavior — resolve space (already Option from T5), ensure space, provision (default dir only, regardless of `~/.oxi/vault` pre-existing — the dir is created), write config.toml with `default_space` only when the file does not exist.

- [ ] **Step 1: Replace `seed_target` and rewrite `run`**

Delete `seed_target` (lines 56–74) and its tests; new body:

```rust
pub async fn run(
    dir: &Path,
    space: &str,
    explicit_dir: bool,
    home: Option<&Path>,
) -> anyhow::Result<()> {
    let name = oxibrain_core::spaces::validate_space_name(space)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(&name).await?;
    println!("initialized brain at {}", dir.display());
    println!("space '{name}' -> {id}");

    if let (Some(h), false) = (home, explicit_dir) {
        let r = provision_space_vault(dir, h, &name)?;
        println!("vault dir: {}", r.vault_dir.display());
        if r.root_added {
            println!("documents root '{name}' added");
        }
        let cfg_path = oxibrain::config::UserConfig::config_path(h);
        if !cfg_path.exists() {
            oxibrain::config::UserConfig::set_default_space(h, &name)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("default space: {name} ({} )", cfg_path.display());
        }
    }

    println!(
        "model weights pull automatically on first extract — pre-fetch with `oxibrain model pull`"
    );
    Ok(())
}
```

- [ ] **Step 2: Tests**

```rust
#[tokio::test]
async fn init_provisions_per_space_vault_and_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    run(dir.path(), "personal", false, Some(home.path())).await.unwrap();
    assert!(home.path().join(".oxi/vault/personal").is_dir());
    assert!(dir.path().join("documents.toml").exists());
    let cfg = std::fs::read_to_string(home.path().join(".oxi/config.toml")).unwrap();
    assert!(cfg.contains("default_space = \"personal\""));
    // Idempotent: second init neither clobbers nor duplicates.
    run(dir.path(), "personal", false, Some(home.path())).await.unwrap();
    let text = std::fs::read_to_string(dir.path().join("documents.toml")).unwrap();
    assert_eq!(text.matches("[[root]]").count(), 1);
}

#[tokio::test]
async fn init_explicit_dir_does_not_touch_foundation() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    run(dir.path(), "work", true, Some(home.path())).await.unwrap();
    assert!(!dir.path().join("documents.toml").exists());
    assert!(!home.path().join(".oxi/config.toml").exists());
}
```

Run: `cargo test -p oxibrain-cli init`
Expected: PASS. Old `seed_target` tests are deleted with the function.

- [ ] **Step 3: Commit**

```bash
git add crates/oxibrain-cli/src/cmd/init.rs
git commit -m "feat: init provisions per-space vault and writes default config"
```

---

### Task 11: MCP — configured default + no implicit creation

**Files:**
- Modify: `crates/oxibrain-mcp/src/server.rs`
- Modify: `crates/oxibrain-cli/src/cmd/serve.rs` (load config, pass default)

**Interfaces:**
- Produces: `BrainServer { default_space: String }` (default `"personal"`); `BrainServer::with_default_space(self, name: String) -> Self`; all `space_arg`-style fallbacks resolve through `self.default_space`; tool handlers resolve spaces via read-only lookup (`resolve_space_id`), never `ensure_space`; unknown space ⇒ tool error with the `space add` hint.

- [ ] **Step 1: State + builder**

In `BrainServer` struct (near `scope: Option<Scope>`, ~line 64) add:

```rust
    /// Default space for tool calls that omit `space` (spec §4.1/§4.6).
    /// Set from `~/.oxi/config.toml` by `serve`; tests default to "personal".
    default_space: String,
```

`from_brain` / `from_brain_scoped`: `default_space: "personal".to_string(),` in the struct literals. Add:

```rust
    pub fn with_default_space(mut self, name: String) -> Self {
        self.default_space = name;
        self
    }
```

- [ ] **Step 2: Resolve without creating**

Replace the private helper (~line 111):

```rust
    /// Resolve a space name to its content-derived ID without creating it
    /// (spec §4.4). Unknown space ⇒ error carrying the `space add` hint.
    async fn resolve_space_id(&self, name: &str) -> Result<String, ToolErr> {
        match self.brain.lookup_space(name).await {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(ToolErr::Params(format!(
                "space '{name}' not found — create it with: oxibrain space add {name}"
            ))),
            Err(e) => Err(ToolErr::run(e)),
        }
    }

    fn default_space(&self) -> &str {
        &self.default_space
    }
```

Change every handler call `self.ensure_space(&space_arg(args)).await?` to `self.resolve_space_id(&self.space_arg(args)).await?` where `space_arg` becomes a method:

```rust
    fn space_arg(&self, args: &Value) -> String {
        args.get("space")
            .and_then(|v| v.as_str())
            .unwrap_or(self.default_space())
            .to_string()
    }
```

(If `space_arg` is a free fn today, move it into the impl; keep any `str_arg` helpers as-is.)

Replace the three inline `unwrap_or("personal")` fallbacks the same way:
- `enforce_scope` (~line 155–158): `.unwrap_or(self.default_space())` — keep the `lookup_space` membership logic unchanged.
- the native-RPC handler ~line 884 (`reproject`/space resolution) and ~line 938 (`document_history`): resolve arg with `.unwrap_or(self.default_space())` and use `resolve_space_id` instead of `ensure_space`.
- `resources/read` space resolution ~line 1204: same.

Delete the private `ensure_space` helper (all call sites converted).

- [ ] **Step 3: serve loads config**

`cmd/serve.rs`: at both `--stdio` and `--http` startup, before constructing the server:

```rust
    let default_space = {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        oxibrain::config::UserConfig::load(home.as_deref())
            .map_err(|e| anyhow::anyhow!("{e}"))? // malformed config: fail serve loudly
            .default_space
    };
    // ... after from_brain(brain):
    let server = server.with_default_space(default_space);
```

(Adapt to serve.rs's actual construction sites; there may be one shared path.)

- [ ] **Step 4: Tests**

In server.rs tests (pattern `fresh_server()` at ~line 2185):

```rust
#[tokio::test]
async fn tool_omitting_space_uses_configured_default() {
    let (dir, server) = fresh_server().await;
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("dev").await.unwrap();
    drop(brain);
    let server = server.with_default_space("dev".into());
    // A read tool with no `space` param must resolve "dev", not create "personal".
    let resp = server.handle(msg(1, "tools/call", Some(serde_json::json!({
        "name": "stats", "arguments": {}
    }).into()))).await.unwrap();
    assert!(resp["result"]["content"].is_array() || resp["result"].is_object());
    // And an unknown explicit space errors with the hint:
    let resp = server.handle(msg(2, "tools/call", Some(serde_json::json!({
        "name": "stats", "arguments": {"space": "ghost"}
    }).into()))).await.unwrap();
    let text = serde_json::to_string(&resp).unwrap();
    assert!(text.contains("oxibrain space add ghost"));
}
```

(Check how existing tool-call tests build the message — mirror the closest `tools/call` test's `msg(...)` shape exactly; `stats` returns JSON content.)

Existing tests that rely on implicit creation via tool calls must create their spaces up front — sweep `cargo test -p oxibrain-mcp` failures and add `brain.ensure_space(...)` to fixtures (the tests already open the brain directly in most cases).

Run: `cargo test -p oxibrain-mcp && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-mcp/src/server.rs crates/oxibrain-cli/src/cmd/serve.rs
git commit -m "feat: MCP resolves configured default space; no implicit creation"
```

---

### Task 12: Documentation + version + full gates

**Files:**
- Modify: `doc/ARCHITECTURE.md`, `doc/CONSUMPTION_CONTRACT.md`, root `Cargo.toml` (workspace version)

- [ ] **Step 1: ARCHITECTURE.md v2.12**

Add a version-note block at the top changelog (same shape as the v2.11 block, above it):

```markdown
> **v2.12 — Space lifecycle (spec 2026-08-27).** Spaces are a managed unit:
> `oxibrain space add|remove|default`, `~/.oxi/config.toml` (`default_space`,
> strict parse, first §18 key implemented), per-space vault provisioning at
> `~/.oxi/vault/<space>/` with legacy flat-root `exclude` fixup, and P5 purge
> via `RedactTarget::Space` (audited; documents cache swept; vault files never
> deleted). Implicit space creation is abolished outside `init`, `space add`,
> and archive imports; the default space resolves as `--space` > config.toml >
> `"personal"` on CLI and MCP alike. Workspace `0.8.0 → 0.9.0`; Consumption
> Contract 1.5.
```

Then edit the body:
- §15.1 append: "**Lifecycle (v2.12).** Spaces are created by `init`/`space add` (with per-space vault provisioning under `~/.oxi/vault/<space>/` for the default brain dir) and removed by `space remove` (empty-only, scaffold-aware) or `space remove --purge` (audited `RedactTarget::Space`; vault files are never deleted — §1.4). The default space lives in `~/.oxi/config.toml` and resolves identically for CLI and MCP."
- §16.4 CLI table: add `oxibrain space add <name>` / `space remove <name> [--purge]` / `space default [<name>]`; update the `spaces` row (documents column, default marker) and note the default-space resolution under the code block.
- §18 `~/.oxi/` tree: under `config.toml` replace the comment with `default_space (v2.12; dir/provider reserved)`; add `vault/<space>/` under `vault`.
- §16.2/§16.1 MCP note: tools omitting `space` resolve the configured default; no implicit creation.

- [ ] **Step 2: CONSUMPTION_CONTRACT.md → 1.5**

Title `# Consumption Contract 1.5`; add a 1.5 section (match the 1.4 section's format):

```markdown
## 1.5 (2026-08-27) — Space lifecycle

1. **Breaking (minor):** verbs and MCP tools no longer implicitly create
   spaces. Create with `oxibrain space add` / `init` / archive import.
   Unknown spaces fail fast with a `space add` hint.
2. **Additive:** MCP tools omitting `space` resolve the default from
   `~/.oxi/config.toml` (`default_space`), not a hardcoded `"personal"`.
3. **Additive:** CLI `space add|remove|default`; `RedactTarget` serde gains
   `{"kind":"space","id":…}`. No new MCP tools; fifteen-tool cap unchanged.
```

- [ ] **Step 3: Version + full gates**

Root `Cargo.toml`: `[workspace.package] version = "0.9.0"`.

```bash
cargo build && cargo test && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1
```

Expected: all pass; the tree grep prints nothing (exit 1 from the chain is the success path — run it as written and expect "no match").

- [ ] **Step 4: Commit**

```bash
git add doc/ARCHITECTURE.md doc/CONSUMPTION_CONTRACT.md Cargo.toml Cargo.lock
git commit -m "docs: architecture v2.12 and consumption contract 1.5 for space lifecycle"
```

---

## Self-Review (completed during planning)

- **Spec coverage:** §4.1 → T2; §4.2 → T1/T6/T7/T9; §4.3 → T7/T10; §4.4 → T5/T11; §4.5 → T3/T4/T8; §4.6 → T11; §4.7 → T1; §5 → T12; §6 mapped per-task tests; §7 → T12.
- **Known soft spots, called out inline:** exact `RootEntry` field defaults (T7 note), `Brain::redact`/`ingest_note` signatures (T8 note), `space_arg` free-fn vs method (T11 note), `msg(...)` test shape (T11 note), `fresh_db` seeded-space id (T4 note). Each instructs the implementer to copy the existing pattern rather than invent.
- **Type consistency:** `UserConfig::resolve_space(flag, home)` used in T5/T11; `cmd::space_id` in T5/T8/T9; `RedactTarget::Space { id }` in T4/T8; `provision_space_vault(brain_dir, home, name)` in T7/T10.
```
