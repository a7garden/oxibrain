//! oxios-memory importer — reads an oxios-memory SQLite database and yields
//! entries for ingestion into oxibrain.
//!
//! This is a one-shot migration tool (DESIGN §16.3). It reads the
//! `memories` table directly — no dependency on the `oxios-memory` crate.
//! The caller is responsible for calling `Brain::ingest` for each entry.
//!
//! Per DESIGN §16.3: entries map to `SourceRef::AgentTrace`, trust `SemiTrusted`.

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// A single entry read from an oxios-memory `memories` table.
///
/// Only the fields needed for migration are extracted. Lifecycle metadata
/// (decay, compaction, access counts) is not carried over — oxibrain's
/// projection derives its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OxiosMemoryEntry {
    /// Original ID from the oxios-memory store (for dedup/logging).
    pub id: String,
    /// Memory type string (episode, fact, preference, etc.).
    pub memory_type: String,
    /// Content text (Markdown).
    pub content: String,
    /// Optional summary.
    pub summary: Option<String>,
    /// Source field (agent name, "compaction", "system", etc.).
    pub source: String,
    /// Tier (cold, warm, hot).
    pub tier: String,
    /// ISO-8601 creation timestamp from the original store.
    pub created_at: String,
}

/// Read all entries from an oxios-memory SQLite database.
///
/// Opens the database read-only. Returns entries ordered by `created_at`
/// ascending so the caller can ingest them in chronological order.
pub fn read_oxios_memory(path: &Path) -> anyhow::Result<Vec<OxiosMemoryEntry>> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| anyhow::anyhow!("open oxios-memory db at {}: {e}", path.display()))?;

    let mut stmt = conn.prepare(
        "SELECT id, memory_type, content, summary, source, tier, created_at \
         FROM memories \
         ORDER BY created_at ASC",
    )?;

    let entries = stmt
        .query_map([], |row| {
            Ok(OxiosMemoryEntry {
                id: row.get(0)?,
                memory_type: row.get(1)?,
                content: row.get(2)?,
                summary: row.get(3)?,
                source: row.get(4)?,
                tier: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OpenFlags;
    use tempfile::tempdir;

    fn build_test_db(dir: &Path) -> std::path::PathBuf {
        let db_path = dir.join("memory.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .expect("open");

        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                memory_type TEXT NOT NULL,
                content TEXT NOT NULL,
                summary TEXT,
                importance REAL NOT NULL DEFAULT 0.5,
                tier TEXT NOT NULL DEFAULT 'warm',
                protection TEXT NOT NULL DEFAULT 'none',
                source TEXT NOT NULL DEFAULT 'unknown',
                session_id TEXT,
                tags TEXT,
                metadata TEXT,
                access_count INTEGER NOT NULL DEFAULT 0,
                pinned INTEGER NOT NULL DEFAULT 0,
                auto_classified INTEGER NOT NULL DEFAULT 0,
                session_appearances INTEGER NOT NULL DEFAULT 0,
                decay_score REAL NOT NULL DEFAULT 1.0,
                compaction_level INTEGER NOT NULL DEFAULT 0,
                content_hash INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                accessed_at TEXT,
                decay_rate REAL NOT NULL DEFAULT 0.01
            );",
        )
        .expect("create table");

        conn.execute(
            "INSERT INTO memories (id, memory_type, content, source, tier, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![
                "mem-001",
                "episode",
                "Discussed Rust async runtime with Alice.",
                "agent",
                "warm",
                "2025-06-01T10:00:00Z",
            ],
        )
        .expect("insert 1");

        conn.execute(
            "INSERT INTO memories (id, memory_type, content, source, tier, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![
                "mem-002",
                "fact",
                "Alice works at Acme Corp.",
                "agent",
                "hot",
                "2025-05-15T08:00:00Z",
            ],
        )
        .expect("insert 2");

        db_path
    }

    #[test]
    fn read_returns_entries_in_chronological_order() {
        let dir = tempdir().expect("tempdir");
        let db = build_test_db(dir.path());

        let entries = read_oxios_memory(&db).expect("read");
        assert_eq!(entries.len(), 2);
        // Chronological: May before June.
        assert_eq!(entries[0].id, "mem-002");
        assert_eq!(entries[1].id, "mem-001");
    }

    #[test]
    fn read_preserves_content_and_metadata() {
        let dir = tempdir().expect("tempdir");
        let db = build_test_db(dir.path());

        let entries = read_oxios_memory(&db).expect("read");
        let first = &entries[0];
        assert_eq!(first.content, "Alice works at Acme Corp.");
        assert_eq!(first.source, "agent");
        assert_eq!(first.tier, "hot");
    }

    #[test]
    fn read_empty_db_returns_empty_vec() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("empty.db");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .expect("open");
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                memory_type TEXT NOT NULL,
                content TEXT NOT NULL,
                summary TEXT,
                importance REAL NOT NULL DEFAULT 0.5,
                tier TEXT NOT NULL DEFAULT 'warm',
                protection TEXT NOT NULL DEFAULT 'none',
                source TEXT NOT NULL DEFAULT 'unknown',
                session_id TEXT,
                tags TEXT,
                metadata TEXT,
                access_count INTEGER NOT NULL DEFAULT 0,
                pinned INTEGER NOT NULL DEFAULT 0,
                auto_classified INTEGER NOT NULL DEFAULT 0,
                session_appearances INTEGER NOT NULL DEFAULT 0,
                decay_score REAL NOT NULL DEFAULT 1.0,
                compaction_level INTEGER NOT NULL DEFAULT 0,
                content_hash INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                accessed_at TEXT,
                decay_rate REAL NOT NULL DEFAULT 0.01
            );",
        )
        .expect("create table");

        let entries = read_oxios_memory(&db_path).expect("read");
        assert!(entries.is_empty());
    }

    #[test]
    fn read_nonexistent_db_returns_error() {
        let dir = tempdir().expect("tempdir");
        let result = read_oxios_memory(&dir.path().join("nonexistent.db"));
        assert!(result.is_err());
    }
}
