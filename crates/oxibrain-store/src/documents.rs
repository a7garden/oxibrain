//! `documents.db` cache plane (Daemonless Two-Plane spec §6).
//!
//! The document cache is physically separate from `brain.db`. It carries
//! verbatim chunk rows, two FTS5 indexes (word + ngram), and an optional
//! vec0 vector table. The brain lock does not protect this database and the
//! documents lock does not protect the brain; the two are independently
//! held by their respective writers.
//!
//! ## Identity
//!
//! - `document_id` and `chunk_id` come from `oxibrain_core::documents`.
//! - `config_hash` is blake3 of the serialized fingerprint fields and is
//!   rewritten on every apply so a config change can be detected and a
//!   root rebuilt.
//!
//! ## Apply
//!
//! One atomic transaction per call. Removal/reset cascades through vectors
//! explicitly, then FTS rows, then chunks/documents/manifest, then the root
//! row. CAS on `doc_roots.generation` rejects stale writers with
//! `BrainError::Busy`. The whole transaction rolls back on any error.

use crate::io_err;
use crate::sql_err;
use fs2::FileExt;
use oxibrain_core::documents::{
    CachedFile, CachedRootMeta, FileAction, PdcProjectionMeta, RootAction, RootFingerprint,
};
use oxibrain_index::{dequantize_i8, quantize_i8_unit};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};
use std::fs::{File, OpenOptions};
use std::path::Path;

/// `documents.db` schema version. Independent of `brain.db`'s ledger version.
pub const DOCUMENTS_SCHEMA_VERSION: i64 = 3;

/// Default embedding dimension for `doc_vectors` (BGE-M3 / multilingual).
/// Mirrors `crate::vectors::EMBEDDING_DIM`.
pub const EMBEDDING_DIM: usize = 1024;

/// Which FTS5 tokenizer the lexical search targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsTable {
    /// `doc_fts_word` — unicode61 tokenizer.
    Word,
    /// `doc_fts_ngram` — trigram tokenizer.
    Ngram,
}

/// Input plan for one atomic apply over `documents.db`.
///
/// `root_actions` is the result of `oxibrain_core::documents::diff_roots`:
/// for each alias present in the configured or cached set, declare whether
/// to keep, reset, or remove it. `roots` then supplies the per-root work
/// to perform after the root-level reset has executed.
#[derive(Debug, Clone)]
pub struct ApplyPlan {
    pub root_actions: Vec<(String, RootAction)>,
    pub roots: Vec<RootApply>,
}

/// Per-root work after any root-level reset has already been applied.
#[derive(Debug, Clone)]
pub struct RootApply {
    pub fingerprint: RootFingerprint,
    pub expected_generation: i64,
    pub actions: Vec<FileAction>,
    /// Ready payloads for `Add`/`Replace`: decoded-text chunks + manifest row.
    pub upserts: Vec<DocumentUpsert>,
}

/// Manifest + chunk payload for one Add/Replace action.
#[derive(Debug, Clone)]
pub struct DocumentUpsert {
    pub locator: String,
    pub revision: String,
    /// `"text/markdown"` | `"text/html"` | `"text/plain"`, for canonical PDC
    /// documents one of the contract media types
    /// `"application/vnd.pdc.document+djot;version=1"` |
    /// `"application/vnd.pdc.document+html;version=1|2"` |
    /// `"application/vnd.pdc.document+markdown;version=2"`, or the query
    /// definition `"application/vnd.pdc.query+yaml;version=1"`.
    pub media_type: String,
    /// Raw file bytes (pre-decode).
    pub bytes: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub modified_ns: i64,
    /// Modification time in seconds (display + ordering).
    pub modified_at: i64,
    /// Final chunk set for this document.
    pub chunks: Vec<ChunkUpsert>,
    /// PDC projection payload when the source decoded as a canonical PDC
    /// document (pdc-adoption-v2); `None` for every legacy decoder.
    pub pdc: Option<PdcProjectionUpsert>,
}

/// PDC projection payload for one canonical PDC document
/// (doc/spec/pdc-adoption-v2.md).
///
/// `uuid` is the canonical envelope UUID and `body_profile` is
/// `'pdc-djot/1'` or `'pdc-html/1'`. `meta` is the decoded envelope
/// metadata; it is stored as JSON in `documents.pdc_meta` and read back
/// for link resolution and the trash filter — never re-decoded from bytes.
#[derive(Debug, Clone)]
pub struct PdcProjectionUpsert {
    pub uuid: String,
    pub body_profile: String,
    pub meta: PdcProjectionMeta,
}

/// One chunk in a document's decoded text.
///
/// Spans address UTF-8 bytes in the decoded slice, not necessarily the raw
/// file. `text` is the verbatim decoded slice — it is indexed in FTS and
/// embedded; it is intentionally not stored in `doc_chunks` (spec §6).
#[derive(Debug, Clone)]
pub struct ChunkUpsert {
    pub ordinal: u32,
    pub span_start: usize,
    pub span_end: usize,
    pub context: String,
    pub text: String,
}

/// View of a chunk as materialized from the cache: spans, ordinal, and the
/// document-level metadata needed to resolve the `doc://` URI.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedChunk {
    pub chunk_id: String,
    pub document_id: String,
    pub root_alias: String,
    pub space: String,
    pub locator: String,
    pub revision: String,
    pub media_type: String,
    pub ordinal: u32,
    pub span_start: usize,
    pub span_end: usize,
    pub modified_at: i64,
}

/// Owns the cache connection and the writer advisory lock (when read-write).
#[derive(Debug)]
pub struct DocumentCache {
    /// SQLite connection to `documents.db`. Public for test inspection;
    /// production callers should go through the typed methods.
    pub conn: Connection,
    pub(crate) lock: Option<DocumentsLock>,
    pub(crate) path: std::path::PathBuf,
}

/// RAII exclusive lock on `<dir>/documents.lock`. Released on drop.
///
/// The file is intentionally NOT removed on drop: unlinking after unlock
/// lets a racing process lock the dying inode while a third one locks a
/// freshly created file, silently breaking mutual exclusion. Same
/// discipline as `crate::lock::AdvisoryLock`.
#[derive(Debug)]
pub struct DocumentsLock {
    _file: File,
    #[allow(dead_code)] // kept for debug/inspection symmetry with the path
    _path: std::path::PathBuf,
}

impl Drop for DocumentsLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}

impl DocumentsLock {
    pub fn acquire(dir: &Path) -> Result<Self, BrainError> {
        std::fs::create_dir_all(dir).map_err(io_err)?;
        let lock_path = dir.join("documents.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(io_err)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self {
                _file: file,
                _path: lock_path,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(BrainError::Busy(format!(
                "another oxibrain process holds {}",
                lock_path.display()
            ))),
            Err(e) => Err(io_err(e)),
        }
    }
}

impl DocumentCache {
    /// Open read-write with an exclusive advisory lock on
    /// `<dir>/documents.lock`. Creates + migrates `documents.db` (schema v1)
    /// if it does not already exist. Lock refusal → `BrainError::Busy`.
    pub fn open_rw(dir: &Path) -> Result<Self, BrainError> {
        crate::migration::ensure_vec_extension();
        let lock = DocumentsLock::acquire(dir)?;
        let db_path = dir.join("documents.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let conn = Connection::open(&db_path).map_err(sql_err)?;
        // Same PRAGMA set as brain.db: WAL mode, FK enforcement, busy timeout,
        // and the same NORMAL synchronous setting. `documents.db` is a
        // separate file so its WAL is independent.
        for p in [
            "PRAGMA journal_mode=WAL;",
            "PRAGMA foreign_keys=ON;",
            "PRAGMA busy_timeout=5000;",
            "PRAGMA synchronous=NORMAL;",
        ] {
            conn.execute_batch(p).map_err(sql_err)?;
        }
        Self::migrate(&conn)?;
        Ok(Self {
            conn,
            lock: Some(lock),
            path: db_path,
        })
    }

    /// Open read-only. No advisory lock; can coexist with a writer process.
    pub fn open_ro(dir: &Path) -> Result<Self, BrainError> {
        crate::migration::ensure_vec_extension();
        let db_path = dir.join("documents.db");
        if !db_path.exists() {
            return Err(BrainError::NotFound(format!(
                "no documents.db at {}",
                db_path.display()
            )));
        }
        let conn = Connection::open(&db_path).map_err(sql_err)?;
        // journal_mode is already WAL (set by open_rw); re-setting it on a
        // query_only connection would fail if it ever needed a write.
        conn.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")
            .map_err(sql_err)?;
        // We do not run migrations on a read-only open. The schema must
        // already exist; if not, the caller initialized it wrong.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sql_err)?;
        if version < 1 {
            return Err(BrainError::NotFound(format!(
                "documents.db at {} has not been migrated",
                db_path.display()
            )));
        }
        Ok(Self {
            conn,
            lock: None,
            path: db_path,
        })
    }

    /// Run the inline v1 migration (idempotent). Sets `PRAGMA user_version`
    /// to `DOCUMENTS_SCHEMA_VERSION`. Independent of brain.db's ledger
    /// version — `documents.db` always starts at v1.
    fn migrate(conn: &Connection) -> Result<(), BrainError> {
        let current: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sql_err)?;
        if current > DOCUMENTS_SCHEMA_VERSION {
            return Err(BrainError::Migration {
                found: current,
                expected: DOCUMENTS_SCHEMA_VERSION,
            });
        }
        if current < 1 {
            conn.execute_batch(V1_SCHEMA_SQL).map_err(sql_err)?;
            conn.pragma_update(None, "user_version", 1i64)
                .map_err(sql_err)?;
        }
        if current < 2 {
            // v2: doc_texts becomes the canonical text store, FTS goes
            // external-content, doc_vectors goes int8 (ADR-014). Existing
            // embeddings are dropped — re-embed via `index --embed`.
            conn.execute_batch(V2_SCHEMA_SQL).map_err(sql_err)?;
            conn.pragma_update(None, "user_version", DOCUMENTS_SCHEMA_VERSION)
                .map_err(sql_err)?;
        }
        if current < 3 {
            // v3: PDC projection columns + the per-root unique UUID index
            // (pdc-adoption-v2). The columns are nullable, so v2 rows
            // survive with NULL and legacy decoders keep writing NULL.
            conn.execute_batch(V3_SCHEMA_SQL).map_err(sql_err)?;
            conn.pragma_update(None, "user_version", DOCUMENTS_SCHEMA_VERSION)
                .map_err(sql_err)?;
        }
        Ok(())
    }

    /// Path of the `documents.db` file this cache owns.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// True when this handle holds the exclusive `documents.lock`
    /// (read-write open); false for read-only opens.
    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Current `documents.db` schema version (`PRAGMA user_version`).
    pub fn user_version(&self) -> Result<i64, BrainError> {
        self.conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(sql_err)
    }

    /// All cached roots as `CachedRootMeta`.
    ///
    /// The full fingerprint is recovered from the serialized copy written
    /// to `cache_meta` at apply time (`root_fp::<alias>`). `doc_roots`
    /// itself stores only the blake3 `config_hash`, so the serialized copy
    /// is what makes `diff_roots(configured, cached)` meaningful — without
    /// it every kept root would look reset and the cache would rebuild on
    /// every query.
    pub fn list_roots(&self) -> Result<Vec<CachedRootMeta>, BrainError> {
        let mut stmt = self
            .conn
            .prepare("SELECT alias, space, generation FROM doc_roots")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (alias, space, generation) = row.map_err(sql_err)?;
            let fingerprint: RootFingerprint = self
                .meta_get(&root_fp_key(&alias))?
                .and_then(|json| serde_json::from_str(&json).ok())
                .ok_or_else(|| {
                    BrainError::Corruption(format!(
                        "doc_roots row {alias} has no serialized fingerprint"
                    ))
                })?;
            out.push(CachedRootMeta {
                alias,
                space,
                fingerprint,
                generation,
            });
        }
        // Deterministic order by alias.
        out.sort_by(|a, b| a.alias.cmp(&b.alias));
        Ok(out)
    }

    /// Cached manifest rows for one root (sorted by locator).
    pub fn root_manifest(&self, alias: &str) -> Result<Vec<CachedFile>, BrainError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT locator, bytes, modified_ns, revision
                 FROM doc_manifest WHERE root_alias = ?1",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![alias], |r| {
                Ok(CachedFile {
                    locator: r.get(0)?,
                    bytes: r.get::<_, i64>(1)? as u64,
                    modified_ns: r.get(2)?,
                    revision: r.get(3)?,
                })
            })
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(sql_err)?);
        }
        out.sort_by(|a, b| a.locator.cmp(&b.locator));
        Ok(out)
    }

    /// Current generation for one root. Returns 0 if the alias has no row.
    pub fn generation(&self, alias: &str) -> Result<i64, BrainError> {
        let row: Result<i64, rusqlite::Error> = self.conn.query_row(
            "SELECT generation FROM doc_roots WHERE alias = ?1",
            params![alias],
            |r| r.get(0),
        );
        match row {
            Ok(g) => Ok(g),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(sql_err(e)),
        }
    }

    /// Resolve a canonical PDC UUID within one root to its cached
    /// `(document_id, locator)`. Read-only; walks the partial unique
    /// index `idx_documents_pdc_uuid` (v3, pdc-adoption-v2). Returns
    /// `None` when the root has no document carrying that UUID — the
    /// normal case for legacy decoders, which never populate the column.
    pub fn resolve_pdc(
        &self,
        root_alias: &str,
        uuid: &str,
    ) -> Result<Option<(String, String)>, BrainError> {
        let row: Result<(String, String), rusqlite::Error> = self.conn.query_row(
            "SELECT id, locator FROM documents
             WHERE root_alias = ?1 AND pdc_document_id = ?2",
            params![root_alias, uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        );
        match row {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(sql_err(e)),
        }
    }

    /// One atomic apply over the whole plan.
    ///
    /// - For each `RemoveRoot`/`ResetRoot`, delete vectors, FTS, documents,
    ///   chunks, manifest, and root rows for that alias (cascade). On
    ///   `ResetRoot` re-insert the root row at generation 1.
    /// - For each `RootApply`, CAS the generation (`expected_generation`
    ///   vs current) → `Busy` on mismatch. Then apply file actions:
    ///   `Delete` removes document + its chunks + vectors + FTS rows;
    ///   `Add`/`Replace` deletes old vectors + FTS rows for that
    ///   document, upserts the documents row, inserts chunks, inserts FTS
    ///   rows for both tokenizers, and upserts the manifest row. On
    ///   success bump generation and rewrite `config_hash`.
    /// - Failure rolls the whole transaction back.
    pub fn apply(&self, plan: &ApplyPlan) -> Result<(), BrainError> {
        let tx = self.conn.unchecked_transaction().map_err(sql_err)?;

        // Phase 1: root-level reset / remove cascades.
        for (alias, action) in &plan.root_actions {
            match action {
                RootAction::KeepRoot => {
                    // Validate generation later in Phase 2 — the row exists
                    // and the alias is in the configured set.
                }
                RootAction::RemoveRoot => {
                    cascade_delete_root(&tx, alias)?;
                    delete_root_fp(&tx, alias)?;
                    tx.execute("DELETE FROM doc_roots WHERE alias = ?1", params![alias])
                        .map_err(sql_err)?;
                }
                RootAction::ResetRoot => {
                    cascade_delete_root(&tx, alias)?;
                    delete_root_fp(&tx, alias)?;
                    tx.execute("DELETE FROM doc_roots WHERE alias = ?1", params![alias])
                        .map_err(sql_err)?;
                    // The new row is written by Phase 2 with generation 1.
                }
            }
        }

        // Phase 2: per-root apply with CAS.
        //
        // The root row is upserted BEFORE the file actions so the FK from
        // `documents.root_alias -> doc_roots.alias` is satisfied at insert
        // time. The whole transaction is still atomic, so a failed CAS
        // or action rolls back the upsert as well.
        for root in &plan.roots {
            let RootApply {
                fingerprint,
                expected_generation,
                actions,
                upserts,
            } = root;
            let alias = fingerprint.alias.clone();

            // CAS: if the row already exists, generation must match.
            // If it doesn't exist, expected_generation must be 0.
            let current: i64 = match tx.query_row(
                "SELECT generation FROM doc_roots WHERE alias = ?1",
                params![&alias],
                |r| r.get(0),
            ) {
                Ok(g) => g,
                Err(rusqlite::Error::QueryReturnedNoRows) => 0,
                Err(e) => return Err(sql_err(e)),
            };
            if current != *expected_generation {
                return Err(BrainError::Busy(format!(
                    "documents.db generation mismatch for {alias}: \
                     cached={current}, expected={expected_generation}"
                )));
            }

            // Upsert the root row at the next generation so the
            // documents-to-root FK is satisfied before any document row
            // is inserted by the file actions below.
            let next = current + 1;
            let config_hash = config_hash_for(fingerprint);
            tx.execute(
                "INSERT INTO doc_roots(alias, space, config_hash, generation, head_revision, scanned_at)
                 VALUES(?1, ?2, ?3, ?4, NULL, ?5)
                 ON CONFLICT(alias) DO UPDATE SET
                   space = excluded.space,
                   config_hash = excluded.config_hash,
                   generation = excluded.generation,
                   scanned_at = excluded.scanned_at",
                params![&alias, &fingerprint.space, &config_hash, next, now_secs()],
            )
            .map_err(sql_err)?;

            // Persist the serialized fingerprint so `list_roots` can
            // return a `CachedRootMeta` that `diff_roots` can compare.
            let fp_json = serde_json::to_string(&fingerprint)
                .map_err(|e| BrainError::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO cache_meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![root_fp_key(&alias), fp_json],
            )
            .map_err(sql_err)?;

            // Apply file actions now that the root row is in place.
            apply_file_actions(&tx, &alias, &fingerprint.space, actions, upserts)?;
        }

        tx.commit().map_err(sql_err)?;
        Ok(())
    }

    /// Lexical search over one FTS5 index. Returns `(chunk_id, bm25 score desc)`.
    pub fn search_fts(
        &self,
        space: &str,
        table: FtsTable,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f64)>, BrainError> {
        let table_name = match table {
            FtsTable::Word => "doc_fts_word",
            FtsTable::Ngram => "doc_fts_ngram",
        };
        // FTS5 implicit-AND query: space-separated tokens. Each token is
        // quoted so FTS5 treats it as a literal phrase — punctuation in
        // the query (`?`, `(`, `:`, `*`, `-`, …) cannot break the MATCH
        // expression with a syntax error. A literal `"` inside a token is
        // escaped by doubling. Mirrors `query::fts_search` (query.rs:466-471).
        let fts_query: String = query
            .split_whitespace()
            .filter(|s| !s.is_empty())
            .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT chunk_id, rank
             FROM {table_name}
             WHERE {table_name} MATCH ?1 AND space = ?2
               AND NOT EXISTS (
                 SELECT 1 FROM doc_chunks dc
                 JOIN documents d ON d.id = dc.document_id
                 WHERE dc.id = {table_name}.chunk_id
                   AND COALESCE(d.pdc_deleted, 0) = 1
               )
             ORDER BY rank
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(sql_err)?;
        let rows = stmt
            .query_map(params![&fts_query, space, limit as i64], |r| {
                let chunk_id: String = r.get(0)?;
                let rank: f64 = r.get(1)?;
                Ok((chunk_id, -rank))
            })
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(sql_err)?);
        }
        Ok(out)
    }

    /// Exact Rust-side L2 KNN over the int8 `doc_vectors` blobs (v2,
    /// ADR-014). Returns `(chunk_id, distance asc)`.
    pub fn knn(
        &self,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f64)>, BrainError> {
        if query_vector.len() != EMBEDDING_DIM {
            return Err(BrainError::Invalid(format!(
                "doc_vectors query dimension mismatch: expected {EMBEDDING_DIM}, got {}",
                query_vector.len()
            )));
        }
        let q = quantize_i8_unit(query_vector);
        let mut stmt = self
            .conn
            .prepare("SELECT chunk_id, embedding FROM doc_vectors")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(sql_err)?;
        let mut scored: Vec<(String, f64)> = Vec::new();
        for row in rows {
            let (chunk_id, blob) = row.map_err(sql_err)?;
            let stored = dequantize_i8(&blob);
            // Exact integer-domain L2 (self-query is exactly 0).
            let dist: f64 = q
                .iter()
                .zip(stored.iter().map(|&b| b as i8 as i64))
                .map(|(&a, sb)| {
                    let d = a as i64 - sb;
                    (d * d) as f64
                })
                .sum::<f64>()
                .sqrt();
            scored.push((chunk_id, dist));
        }
        // Trash semantics (pdc-adoption-v2): canonical documents whose
        // envelope says deleted:true stay indexed but out of default
        // retrieval, on the dense channel exactly like the lexical one.
        let mut trashed = self
            .conn
            .prepare(
                "SELECT 1 FROM doc_chunks dc
                 JOIN documents d ON d.id = dc.document_id
                 WHERE dc.id = ?1 AND COALESCE(d.pdc_deleted, 0) = 1",
            )
            .map_err(sql_err)?;
        let mut kept: Vec<(String, f64)> = Vec::with_capacity(scored.len());
        for (chunk_id, dist) in scored {
            match trashed.query_row(params![&chunk_id], |_r| Ok(())) {
                Ok(()) => {}
                Err(rusqlite::Error::QueryReturnedNoRows) => kept.push((chunk_id, dist)),
                Err(e) => return Err(sql_err(e)),
            }
        }
        kept.sort_by(|a, b| a.1.total_cmp(&b.1));
        kept.truncate(limit);
        Ok(kept)
    }

    /// Load the cached chunk rows for a set of chunk IDs (order preserved).
    pub fn chunks(&self, ids: &[&str]) -> Result<Vec<CachedChunk>, BrainError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // Build "?, ?, ?" placeholders for the IN list.
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!(
            "SELECT c.id, c.document_id, d.root_alias, c.space, d.locator,
                    d.revision, d.media_type, c.ordinal,
                    c.span_start, c.span_end, d.modified_at
             FROM doc_chunks c
             JOIN documents d ON d.id = c.document_id
             WHERE c.id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(sql_err)?;
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(&*params_vec, |r| {
                Ok(CachedChunk {
                    chunk_id: r.get(0)?,
                    document_id: r.get(1)?,
                    root_alias: r.get(2)?,
                    space: r.get(3)?,
                    locator: r.get(4)?,
                    revision: r.get(5)?,
                    media_type: r.get(6)?,
                    ordinal: r.get(7)?,
                    span_start: r.get::<_, i64>(8)? as usize,
                    span_end: r.get::<_, i64>(9)? as usize,
                    modified_at: r.get(10)?,
                })
            })
            .map_err(sql_err)?;
        // Build a position index so the caller gets the same order they
        // asked for. Unknown ids are skipped (do not error).
        let mut by_id: std::collections::HashMap<String, CachedChunk> =
            std::collections::HashMap::new();
        for row in rows {
            let chunk = row.map_err(sql_err)?;
            by_id.insert(chunk.chunk_id.clone(), chunk);
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(c) = by_id.remove(*id) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// `(embedded, total)` chunk counts for one space.
    /// `embedded` is the count of chunks in that space that have a row in
    /// `doc_vectors`; `total` is the count of all chunks in that space.
    pub fn embedded_count(&self, space: &str) -> Result<(u64, u64), BrainError> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE space = ?1",
                params![space],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        let embedded: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*)
                 FROM doc_chunks c
                 JOIN doc_vectors v ON v.chunk_id = c.id
                 WHERE c.space = ?1",
                params![space],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        Ok((embedded as u64, total as u64))
    }

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
            "DELETE FROM doc_fts_word WHERE rowid IN (SELECT rowid FROM doc_texts WHERE space = ?1)",
            "DELETE FROM doc_fts_ngram WHERE rowid IN (SELECT rowid FROM doc_texts WHERE space = ?1)",
            "DELETE FROM doc_texts WHERE space = ?1",
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

    /// Chunks in `space` without a vector row yet. Returns `(chunk_id, text)`
    /// where `text` is recovered from `doc_fts_word.body` (the canonical
    /// decoded-text store; `doc_chunks` deliberately does not duplicate it).
    pub fn pending_vector_chunks(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, BrainError> {
        // doc_texts is the canonical decoded-text store (v2, ADR-014).
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.id, t.body
                 FROM doc_chunks c
                 JOIN doc_texts t ON t.chunk_id = c.id
                 WHERE c.space = ?1
                   AND NOT EXISTS (
                     SELECT 1 FROM doc_vectors v WHERE v.chunk_id = c.id
                   )
                 ORDER BY c.id
                 LIMIT ?2",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space, limit as i64], |r| {
                let chunk_id: String = r.get(0)?;
                let body: String = r.get(1)?;
                Ok((chunk_id, body))
            })
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(sql_err)?);
        }
        Ok(out)
    }

    /// Upsert embedding vectors for chunks. Overwrites any existing vector
    /// for the same `chunk_id`. Mirrors `crate::vectors::upsert_vector`.
    pub fn upsert_vectors(&self, rows: &[(String, Vec<f32>)]) -> Result<(), BrainError> {
        let tx = self.conn.unchecked_transaction().map_err(sql_err)?;
        for (chunk_id, embedding) in rows {
            if embedding.len() != EMBEDDING_DIM {
                return Err(BrainError::Invalid(format!(
                    "doc_vectors upsert dimension mismatch for {chunk_id}: \
                     expected {EMBEDDING_DIM}, got {}",
                    embedding.len()
                )));
            }
            // vec0 doesn't support INSERT OR REPLACE — DELETE then INSERT.
            tx.execute(
                "DELETE FROM doc_vectors WHERE chunk_id = ?1",
                params![chunk_id],
            )
            .map_err(sql_err)?;
            tx.execute(
                "INSERT INTO doc_vectors(chunk_id, embedding) VALUES (?1, ?2)",
                params![chunk_id, quantize_i8_unit(embedding)],
            )
            .map_err(sql_err)?;
        }
        tx.commit().map_err(sql_err)?;
        Ok(())
    }

    /// Delete every row in `doc_vectors`. Used when the embedding model
    /// identity changes (spec §6 / invariants §11).
    pub fn clear_vectors(&self) -> Result<(), BrainError> {
        self.conn
            .execute("DELETE FROM doc_vectors", [])
            .map_err(sql_err)?;
        Ok(())
    }

    /// Read a key from `cache_meta`. Mirrors `crate::meta::get` but on the
    /// `documents.db` `cache_meta` table.
    pub fn meta_get(&self, key: &str) -> Result<Option<String>, BrainError> {
        let row: Result<String, rusqlite::Error> = self.conn.query_row(
            "SELECT value FROM cache_meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(sql_err(e)),
        }
    }

    /// Write a key into `cache_meta`. Mirrors `crate::meta::set`.
    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), BrainError> {
        self.conn
            .execute(
                "INSERT INTO cache_meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(sql_err)?;
        Ok(())
    }
}

/// `documents.db` v1 schema (spec §6, verbatim, plus `media_type` on
/// `documents`). Applied verbatim by `DocumentCache::migrate` when
/// `PRAGMA user_version` is below 1.
const V1_SCHEMA_SQL: &str = include_str!("documents_v1.sql");

/// v2 schema migration (ADR-014): doc_texts + external-content FTS +
/// plain int8 doc_vectors.
const V2_SCHEMA_SQL: &str = include_str!("documents_v2.sql");

/// v3 schema migration (pdc-adoption-v2): canonical PDC identity columns
/// on `documents` + a partial unique index making a UUID unique within
/// one root.
const V3_SCHEMA_SQL: &str = include_str!("documents_v3.sql");

/// Monotonic timestamp used for `doc_roots.scanned_at`. Seconds since the
/// Unix epoch; the column has no sub-second precision.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Hash the fingerprint fields deterministically. Output is blake3 hex;
/// changes whenever any input field changes.
fn config_hash_for(fp: &RootFingerprint) -> String {
    let mut h = blake3::Hasher::new();
    for (k, v) in [
        ("alias", fp.alias.as_str()),
        ("canonical_path", fp.canonical_path.as_str()),
        ("space", fp.space.as_str()),
    ] {
        h.update(k.as_bytes());
        h.update(&[0u8]);
        h.update(v.as_bytes());
        h.update(&[0u8]);
    }
    let include = format!("{:?}", fp.include);
    let exclude = format!("{:?}", fp.exclude);
    h.update(b"include");
    h.update(&[0u8]);
    h.update(include.as_bytes());
    h.update(&[0u8]);
    h.update(b"exclude");
    h.update(&[0u8]);
    h.update(exclude.as_bytes());
    h.update(&[0u8]);
    h.update(b"max_file_bytes");
    h.update(&[0u8]);
    h.update(&fp.max_file_bytes.to_le_bytes());
    h.update(&[0u8]);
    let mut out = [0u8; 32];
    h.finalize_xof().fill(&mut out);
    hex::encode(out)
}
/// `cache_meta` key holding the serialized fingerprint for one root.
fn root_fp_key(alias: &str) -> String {
    format!("root_fp::{alias}")
}

/// Delete the serialized fingerprint for one root (inside apply's tx).
fn delete_root_fp(tx: &rusqlite::Transaction<'_>, alias: &str) -> Result<(), BrainError> {
    tx.execute(
        "DELETE FROM cache_meta WHERE key = ?1",
        params![root_fp_key(alias)],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Remove every row owned by `alias` from the cache, in the right order:
/// vectors first (no FK possible), then FTS rows, then chunks/documents
/// via ON DELETE CASCADE on `documents.root_alias` → `doc_chunks.document_id`
/// → `doc_manifest.root_alias`.
fn cascade_delete_root(tx: &rusqlite::Transaction<'_>, alias: &str) -> Result<(), BrainError> {
    // 1. vectors for chunks of any document in this root.
    tx.execute(
        "DELETE FROM doc_vectors
         WHERE chunk_id IN (
           SELECT c.id FROM doc_chunks c
           JOIN documents d ON d.id = c.document_id
           WHERE d.root_alias = ?1
         )",
        params![alias],
    )
    .map_err(sql_err)?;
    // 2. FTS rows for the same chunks.
    delete_chunk_texts(
        tx,
        "SELECT c.id FROM doc_chunks c
           JOIN documents d ON d.id = c.document_id
           WHERE d.root_alias = ?1",
        &[&alias],
    )?;
    // 3. documents row cascades through doc_chunks + doc_manifest via FK.
    tx.execute(
        "DELETE FROM documents WHERE root_alias = ?1",
        params![alias],
    )
    .map_err(sql_err)?;
    // 4. manifest is on a separate FK from doc_roots; clean up explicitly
    // to avoid orphan rows if ON DELETE CASCADE is missing on the legacy
    // migration. Cheap and idempotent.
    tx.execute(
        "DELETE FROM doc_manifest WHERE root_alias = ?1",
        params![alias],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Delete FTS index rows + the canonical doc_texts rows for a chunk set.
/// `chunk_filter` is a `(SELECT id FROM doc_chunks ...)` subquery; `params`
/// bind it. Order matters: the external-content FTS deletes read the old
/// bodies from doc_texts (v2, ADR-014), so doc_texts is cleared last.
fn delete_chunk_texts(
    conn: &rusqlite::Connection,
    chunk_filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<(), BrainError> {
    let rowids: Vec<i64> = {
        let sql = format!("SELECT rowid FROM doc_texts WHERE chunk_id IN ({chunk_filter})");
        let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
        stmt.query_map(params, |r| r.get(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?
    };
    for rowid in &rowids {
        conn.execute("DELETE FROM doc_fts_word WHERE rowid = ?1", params![rowid])
            .map_err(sql_err)?;
        conn.execute("DELETE FROM doc_fts_ngram WHERE rowid = ?1", params![rowid])
            .map_err(sql_err)?;
    }
    let sql = format!("DELETE FROM doc_texts WHERE chunk_id IN ({chunk_filter})");
    conn.execute(&sql, params).map_err(sql_err)?;
    Ok(())
}

/// Apply the per-file actions for one root inside an open transaction.
fn apply_file_actions(
    tx: &rusqlite::Transaction<'_>,
    alias: &str,
    space: &str,
    actions: &[FileAction],
    upserts: &[DocumentUpsert],
) -> Result<(), BrainError> {
    // Build a map: locator -> DocumentUpsert for O(1) lookup.
    let upsert_by_loc: std::collections::HashMap<&str, &DocumentUpsert> =
        upserts.iter().map(|u| (u.locator.as_str(), u)).collect();

    for action in actions {
        match action {
            FileAction::Unchanged => {
                // Nothing to write — the document row, chunks, vectors, FTS,
                // and manifest row are all assumed current. Apply-stage
                // reports separately collect any Skip records the
                // connector surfaced.
            }
            FileAction::Delete { locator } => {
                delete_document(tx, alias, locator)?;
            }
            FileAction::Add(obs) | FileAction::Replace(obs) => {
                let upsert = upsert_by_loc.get(obs.locator.as_str()).ok_or_else(|| {
                    BrainError::Invalid(format!(
                        "apply: Add/Replace for {alias}/{} has no matching DocumentUpsert",
                        obs.locator
                    ))
                })?;
                upsert_document(tx, alias, space, upsert)?;
            }
            FileAction::Skip {
                locator: _,
                reason: _,
            } => {
                // Skipped files are surfaced by the connector separately; the
                // pure planner never emits Skip, so this is recorded only at
                // the apply-stage report boundary (out of scope for this
                // store module).
            }
        }
    }
    Ok(())
}

/// Delete one document and everything reachable from it (vectors, FTS,
/// chunks via FK cascade, manifest row).
fn delete_document(
    tx: &rusqlite::Transaction<'_>,
    alias: &str,
    locator: &str,
) -> Result<(), BrainError> {
    // 1. vectors for any chunk of this document.
    tx.execute(
        "DELETE FROM doc_vectors
         WHERE chunk_id IN (
           SELECT c.id FROM doc_chunks c
           JOIN documents d ON d.id = c.document_id
           WHERE d.root_alias = ?1 AND d.locator = ?2
         )",
        params![alias, locator],
    )
    .map_err(sql_err)?;
    // 2. FTS rows for the same chunks.
    delete_chunk_texts(
        tx,
        "SELECT c.id FROM doc_chunks c
           JOIN documents d ON d.id = c.document_id
           WHERE d.root_alias = ?1 AND d.locator = ?2",
        &[&alias, &locator],
    )?;
    // 3. chunks + manifest via documents cascade.
    tx.execute(
        "DELETE FROM documents WHERE root_alias = ?1 AND locator = ?2",
        params![alias, locator],
    )
    .map_err(sql_err)?;
    tx.execute(
        "DELETE FROM doc_manifest WHERE root_alias = ?1 AND locator = ?2",
        params![alias, locator],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Insert/replace one document: delete any prior vectors + FTS for the
/// document (which won't exist for Add; will exist for Replace), upsert the
/// documents row, insert chunks, insert FTS rows for both tokenizers, and
/// upsert the manifest row.
fn upsert_document(
    tx: &rusqlite::Transaction<'_>,
    alias: &str,
    space: &str,
    upsert: &DocumentUpsert,
) -> Result<(), BrainError> {
    let document_id = oxibrain_core::documents::document_id(alias, &upsert.locator);

    // 1. Clear prior vectors + FTS for any existing chunk of this document
    // so a Replace produces new chunk ids (revision-keyed) cleanly.
    tx.execute(
        "DELETE FROM doc_vectors
         WHERE chunk_id IN (
           SELECT id FROM doc_chunks WHERE document_id = ?1
         )",
        params![&document_id],
    )
    .map_err(sql_err)?;
    delete_chunk_texts(
        tx,
        "SELECT id FROM doc_chunks WHERE document_id = ?1",
        &[&document_id],
    )?;

    // 2. Delete prior chunk + documents + manifest rows (Replace path).
    tx.execute(
        "DELETE FROM doc_chunks WHERE document_id = ?1",
        params![&document_id],
    )
    .map_err(sql_err)?;
    tx.execute("DELETE FROM documents WHERE id = ?1", params![&document_id])
        .map_err(sql_err)?;
    tx.execute(
        "DELETE FROM doc_manifest WHERE root_alias = ?1 AND locator = ?2",
        params![alias, &upsert.locator],
    )
    .map_err(sql_err)?;

    // 3. Insert documents row. The PDC projection columns (v3) are NULL
    // across the board for legacy decoders; a canonical PDC upsert writes
    // the envelope identity, the serialized metadata, and the trash flag
    // (1 = envelope deleted:true, 0 = live).
    let (pdc_uuid, pdc_profile, pdc_meta, pdc_deleted) = match &upsert.pdc {
        Some(pdc) => {
            let meta_json = serde_json::to_string(&pdc.meta)
                .map_err(|e| BrainError::Storage(format!("serialize pdc_meta: {e}")))?;
            (
                Some(pdc.uuid.as_str()),
                Some(pdc.body_profile.as_str()),
                Some(meta_json),
                Some(if pdc.meta.deleted { 1i64 } else { 0i64 }),
            )
        }
        None => (None, None, None, None),
    };
    if let Err(e) = tx.execute(
        "INSERT INTO documents
           (id, root_alias, space, locator, revision, media_type,
            bytes, modified_at, indexed_at,
            pdc_document_id, pdc_body_profile, pdc_meta, pdc_deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            &document_id,
            alias,
            space,
            &upsert.locator,
            &upsert.revision,
            &upsert.media_type,
            upsert.bytes as i64,
            upsert.modified_at,
            now_secs(),
            pdc_uuid,
            pdc_profile,
            pdc_meta,
            pdc_deleted,
        ],
    ) {
        return Err(map_document_insert_err(tx, alias, upsert, e));
    }

    // 4. Insert chunks + both FTS rows.
    for chunk in &upsert.chunks {
        let chunk_id =
            oxibrain_core::documents::chunk_id(&document_id, &upsert.revision, chunk.ordinal);
        tx.execute(
            "INSERT INTO doc_chunks
               (id, document_id, space, ordinal, span_start, span_end, context)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &chunk_id,
                &document_id,
                space,
                chunk.ordinal as i64,
                chunk.span_start as i64,
                chunk.span_end as i64,
                &chunk.context,
            ],
        )
        .map_err(sql_err)?;
        // doc_texts is the canonical copy; the FTS tables are
        // external-content indexes over it (v2, ADR-014).
        tx.execute(
            "INSERT INTO doc_texts(body, space, chunk_id) VALUES (?1, ?2, ?3)",
            params![&chunk.text, space, &chunk_id],
        )
        .map_err(sql_err)?;
        let rowid = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO doc_fts_word(rowid, body, space, chunk_id) VALUES (?1, ?2, ?3, ?4)",
            params![rowid, &chunk.text, space, &chunk_id],
        )
        .map_err(sql_err)?;
        tx.execute(
            "INSERT INTO doc_fts_ngram(rowid, body, space, chunk_id) VALUES (?1, ?2, ?3, ?4)",
            params![rowid, &chunk.text, space, &chunk_id],
        )
        .map_err(sql_err)?;
    }

    // 5. Manifest row.
    tx.execute(
        "INSERT INTO doc_manifest(root_alias, locator, bytes, modified_ns, revision)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            alias,
            &upsert.locator,
            upsert.bytes as i64,
            upsert.modified_ns,
            &upsert.revision,
        ],
    )
    .map_err(sql_err)?;

    Ok(())
}

/// Map a failed `documents` insert to a typed error (inside apply's tx).
///
/// A UNIQUE violation on `idx_documents_pdc_uuid` means two documents in
/// one root claim the same canonical UUID. The facade filters these
/// conflicts before apply, so reaching this is defensive; it must surface
/// as `BrainError::Invalid` naming both locators, not as generic Storage
/// noise. Every other failure passes through as `BrainError::Storage`.
fn map_document_insert_err(
    tx: &rusqlite::Transaction<'_>,
    alias: &str,
    upsert: &DocumentUpsert,
    err: rusqlite::Error,
) -> BrainError {
    // Message shape: "UNIQUE constraint failed: documents.root_alias,
    // documents.pdc_document_id".
    let uuid_conflict = matches!(
        &err,
        rusqlite::Error::SqliteFailure(_, Some(msg))
            if msg.contains("UNIQUE constraint failed") && msg.contains("pdc_document_id")
    );
    if !uuid_conflict {
        return sql_err(err);
    }
    let Some(pdc) = upsert.pdc.as_ref() else {
        return sql_err(err);
    };
    // The constraint aborts only the failing statement, so the row that
    // already holds the UUID is still readable — name its locator.
    let existing: Option<String> = tx
        .query_row(
            "SELECT locator FROM documents
             WHERE root_alias = ?1 AND pdc_document_id = ?2",
            params![alias, &pdc.uuid],
            |r| r.get(0),
        )
        .ok();
    let conflicting = &upsert.locator;
    match existing {
        Some(existing) => BrainError::Invalid(format!(
            "duplicate PDC document id {} in root '{alias}': already indexed at '{existing}', conflicting upsert for '{conflicting}'",
            pdc.uuid
        )),
        None => BrainError::Invalid(format!(
            "duplicate PDC document id {} in root '{alias}': conflicting upsert for '{conflicting}'",
            pdc.uuid
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxibrain_core::documents::FileObservation;

    /// Open a tempdir-backed read-write cache. Returns the cache and the
    /// dir so the caller can keep it alive for the test duration.
    fn temp_cache() -> (tempfile::TempDir, DocumentCache) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DocumentCache::open_rw(dir.path()).expect("open_rw");
        (dir, cache)
    }

    fn fp(alias: &str, space: &str) -> RootFingerprint {
        RootFingerprint {
            alias: alias.to_owned(),
            canonical_path: format!("/tmp/{alias}"),
            space: space.to_owned(),
            include: vec!["**/*.md".to_owned()],
            exclude: Vec::new(),
            max_file_bytes: 1024 * 1024,
            decoder_version: "2".to_owned(),
        }
    }

    fn obs(locator: &str, bytes: u64, rev: &str) -> FileObservation {
        FileObservation {
            locator: locator.to_owned(),
            bytes,
            modified_ns: 1_700_000_000_000_000_000,
            revision_hint: Some(rev.to_owned()),
        }
    }

    fn upsert(locator: &str, revision: &str, text: &str, chunks: usize) -> DocumentUpsert {
        let mut cu = Vec::with_capacity(chunks);
        let per = text.len() / chunks.max(1);
        for i in 0..chunks {
            let s = i * per;
            let e = if i + 1 == chunks {
                text.len()
            } else {
                (i + 1) * per
            };
            cu.push(ChunkUpsert {
                ordinal: i as u32,
                span_start: s,
                span_end: e,
                context: String::new(),
                text: text[s..e].to_owned(),
            });
        }
        DocumentUpsert {
            locator: locator.to_owned(),
            revision: revision.to_owned(),
            media_type: "text/markdown".to_owned(),
            bytes: text.len() as u64,
            modified_ns: 1_700_000_000_000_000_000,
            modified_at: 1_700_000_000,
            chunks: cu,
            pdc: None,
        }
    }

    /// Build a 1-root Add plan with one document. Returns the plan and the
    /// matching `DocumentUpsert` for later assertions.
    fn add_plan(alias: &str, space: &str, locator: &str, revision: &str) -> ApplyPlan {
        let fingerprint = fp(alias, space);
        let observation = obs(locator, 12, revision);
        let upserts = vec![upsert(locator, revision, "hello world", 1)];
        let actions = vec![FileAction::Add(observation)];
        ApplyPlan {
            root_actions: vec![(alias.to_owned(), RootAction::KeepRoot)],
            roots: vec![RootApply {
                fingerprint,
                expected_generation: 0,
                actions,
                upserts,
            }],
        }
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .expect("count")
    }

    #[test]
    fn open_rw_creates_schema() {
        let (_dir, cache) = temp_cache();
        assert_eq!(cache.user_version().unwrap(), DOCUMENTS_SCHEMA_VERSION);
        let roots = cache.list_roots().unwrap();
        assert!(roots.is_empty());
        let version = cache.meta_get("documents_schema_version").unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn open_rw_second_open_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let _first = DocumentCache::open_rw(dir.path()).unwrap();
        let second = DocumentCache::open_rw(dir.path());
        assert!(matches!(second, Err(BrainError::Busy(_))));
    }

    #[test]
    fn open_ro_fails_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = DocumentCache::open_ro(dir.path()).unwrap_err();
        assert!(matches!(err, BrainError::NotFound(_)));
    }

    #[test]
    fn apply_add_inserts_everywhere() {
        let (_dir, cache) = temp_cache();
        let plan = add_plan("vault", "personal", "notes/a.md", "rev1");
        cache.apply(&plan).unwrap();

        // documents
        assert_eq!(count(&cache.conn, "documents"), 1);
        // chunks
        assert_eq!(count(&cache.conn, "doc_chunks"), 1);
        // both FTS tables
        assert_eq!(count(&cache.conn, "doc_fts_word"), 1);
        assert_eq!(count(&cache.conn, "doc_fts_ngram"), 1);
        // manifest
        assert_eq!(count(&cache.conn, "doc_manifest"), 1);
        // doc_vectors is empty until upserted
        assert_eq!(count(&cache.conn, "doc_vectors"), 0);
        // one root at gen 1
        assert_eq!(cache.generation("vault").unwrap(), 1);
    }

    #[test]
    fn apply_replace_removes_old_chunks_and_inserts_new() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();
        let old_chunk_id = {
            let conn = &cache.conn;
            conn.query_row(
                "SELECT id FROM doc_chunks ORDER BY ordinal LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
        };

        // Replace with a new revision that has 2 chunks instead of 1.
        let fingerprint = fp("vault", "personal");
        let observation = obs("notes/a.md", 100, "rev2");
        let new_upserts = vec![upsert("notes/a.md", "rev2", "the quick brown fox", 2)];
        let plan = ApplyPlan {
            root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
            roots: vec![RootApply {
                fingerprint,
                expected_generation: 1,
                actions: vec![FileAction::Replace(observation)],
                upserts: new_upserts,
            }],
        };
        cache.apply(&plan).unwrap();

        // Old chunk gone; new chunks present.
        let still_old: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE id = ?1",
                params![&old_chunk_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_old, 0, "old chunk row survived Replace");
        assert_eq!(count(&cache.conn, "doc_chunks"), 2);
        // Both FTS tables reflect the new text only — no stale rows.
        assert_eq!(count(&cache.conn, "doc_fts_word"), 2);
        assert_eq!(count(&cache.conn, "doc_fts_ngram"), 2);
        // Generation bumped to 2.
        assert_eq!(cache.generation("vault").unwrap(), 2);
    }

    #[test]
    fn apply_delete_removes_everything() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();
        let fingerprint = fp("vault", "personal");
        let plan = ApplyPlan {
            root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
            roots: vec![RootApply {
                fingerprint,
                expected_generation: 1,
                actions: vec![FileAction::Delete {
                    locator: "notes/a.md".to_owned(),
                }],
                upserts: Vec::new(),
            }],
        };
        cache.apply(&plan).unwrap();

        assert_eq!(count(&cache.conn, "documents"), 0);
        assert_eq!(count(&cache.conn, "doc_chunks"), 0);
        assert_eq!(count(&cache.conn, "doc_fts_word"), 0);
        assert_eq!(count(&cache.conn, "doc_fts_ngram"), 0);
        assert_eq!(count(&cache.conn, "doc_manifest"), 0);
    }

    #[test]
    fn remove_root_cascades_through_vectors_and_fts() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();

        // Insert a fake vector to verify cascade.
        let chunk_id: String = cache
            .conn
            .query_row(
                "SELECT id FROM doc_chunks ORDER BY ordinal LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let emb = vec![0.0f32; EMBEDDING_DIM];
        cache
            .upsert_vectors(&[(chunk_id.clone(), emb.clone())])
            .unwrap();
        assert_eq!(count(&cache.conn, "doc_vectors"), 1);

        // Now Remove the root.
        let plan = ApplyPlan {
            root_actions: vec![("vault".to_owned(), RootAction::RemoveRoot)],
            roots: Vec::new(),
        };
        cache.apply(&plan).unwrap();

        assert_eq!(count(&cache.conn, "documents"), 0);
        assert_eq!(count(&cache.conn, "doc_chunks"), 0);
        assert_eq!(count(&cache.conn, "doc_fts_word"), 0);
        assert_eq!(count(&cache.conn, "doc_fts_ngram"), 0);
        assert_eq!(count(&cache.conn, "doc_manifest"), 0);
        assert_eq!(count(&cache.conn, "doc_vectors"), 0);
        // Root row gone too.
        let roots = cache.list_roots().unwrap();
        assert!(roots.is_empty());
    }

    #[test]
    fn reset_root_rebuilds_at_generation_one() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();
        assert_eq!(cache.generation("vault").unwrap(), 1);

        // Reset clears everything, then we re-add at generation 1.
        let fingerprint = fp("vault", "personal");
        let new_observation = obs("notes/a.md", 8, "rev9");
        let new_upserts = vec![upsert("notes/a.md", "rev9", "rewritten", 1)];
        let plan = ApplyPlan {
            root_actions: vec![("vault".to_owned(), RootAction::ResetRoot)],
            roots: vec![RootApply {
                fingerprint,
                expected_generation: 0,
                actions: vec![FileAction::Add(new_observation)],
                upserts: new_upserts,
            }],
        };
        cache.apply(&plan).unwrap();

        assert_eq!(count(&cache.conn, "documents"), 1);
        assert_eq!(count(&cache.conn, "doc_chunks"), 1);
        // Generation is 1 again — the root was reset.
        assert_eq!(cache.generation("vault").unwrap(), 1);
    }

    #[test]
    fn cas_mismatch_returns_busy_and_no_partial_state() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();

        // Caller's snapshot expected gen 0 (a stale view).
        let fingerprint = fp("vault", "personal");
        let observation = obs("notes/a.md", 8, "rev2");
        let upserts = vec![upsert("notes/a.md", "rev2", "rewritten", 1)];
        let plan = ApplyPlan {
            root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
            roots: vec![RootApply {
                fingerprint,
                expected_generation: 0,
                actions: vec![FileAction::Replace(observation)],
                upserts,
            }],
        };
        let err = cache.apply(&plan).unwrap_err();
        assert!(matches!(err, BrainError::Busy(_)));

        // No partial state: every row still reflects the rev1 add.
        assert_eq!(count(&cache.conn, "documents"), 1);
        assert_eq!(count(&cache.conn, "doc_chunks"), 1);
        assert_eq!(count(&cache.conn, "doc_fts_word"), 1);
        assert_eq!(count(&cache.conn, "doc_fts_ngram"), 1);
        assert_eq!(count(&cache.conn, "doc_manifest"), 1);
        // Generation is still 1 — the failed CAS did not bump.
        assert_eq!(cache.generation("vault").unwrap(), 1);
    }

    #[test]
    fn rebuild_equivalence_after_drop_and_rebuild() {
        // Build one cache, snapshot row counts.
        let (dir1, cache1) = temp_cache();
        cache1
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();
        let snap1 = snapshot_counts(&cache1);
        drop(cache1);
        // Drop the file from disk.
        let _ = std::fs::remove_file(dir1.path().join("documents.db"));
        let _ = std::fs::remove_file(dir1.path().join("documents.db-wal"));
        let _ = std::fs::remove_file(dir1.path().join("documents.db-shm"));
        // Reopen + reapply identical plan.
        let cache2 = DocumentCache::open_rw(dir1.path()).unwrap();
        cache2
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();
        let snap2 = snapshot_counts(&cache2);
        assert_eq!(snap1, snap2);
    }

    fn snapshot_counts(cache: &DocumentCache) -> [i64; 6] {
        [
            count(&cache.conn, "documents"),
            count(&cache.conn, "doc_chunks"),
            count(&cache.conn, "doc_fts_word"),
            count(&cache.conn, "doc_fts_ngram"),
            count(&cache.conn, "doc_manifest"),
            count(&cache.conn, "doc_vectors"),
        ]
    }

    #[test]
    fn embedded_count_pending_clear_vectors() {
        let (_dir, cache) = temp_cache();
        // Index two documents with two chunks each.
        let plan = {
            let fingerprint = fp("vault", "personal");
            let upserts = vec![
                upsert("notes/a.md", "rev1", "alpha bravo", 2),
                upsert("notes/b.md", "rev1", "charlie delta", 2),
            ];
            let actions = upserts
                .iter()
                .map(|u| FileAction::Add(obs(&u.locator, u.bytes, &u.revision)))
                .collect();
            ApplyPlan {
                root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
                roots: vec![RootApply {
                    fingerprint,
                    expected_generation: 0,
                    actions,
                    upserts,
                }],
            }
        };
        cache.apply(&plan).unwrap();

        let (embedded, total) = cache.embedded_count("personal").unwrap();
        assert_eq!(embedded, 0);
        assert_eq!(total, 4);

        let pending = cache.pending_vector_chunks("personal", 100).unwrap();
        assert_eq!(pending.len(), 4);

        // Embed one chunk. The remaining pending rows must not contain the
        // embedded id, and every recovered text must be one of the chunk
        // texts the plan wrote.
        let (chunk_id, _text) = pending[0].clone();
        let expected_texts: std::collections::HashSet<String> = plan.roots[0]
            .upserts
            .iter()
            .flat_map(|u| u.chunks.iter().map(|c| c.text.clone()))
            .collect();
        cache
            .upsert_vectors(&[(chunk_id.clone(), vec![0.0f32; EMBEDDING_DIM])])
            .unwrap();
        let (embedded, total) = cache.embedded_count("personal").unwrap();
        assert_eq!(embedded, 1);
        assert_eq!(total, 4);
        let pending = cache.pending_vector_chunks("personal", 100).unwrap();
        assert_eq!(pending.len(), 3);
        assert!(pending.iter().all(|(id, _)| id != &chunk_id));
        assert!(pending.iter().all(|(_, t)| expected_texts.contains(t)));

        // clear_vectors wipes the channel but leaves chunks.
        cache.clear_vectors().unwrap();
        let (embedded, total) = cache.embedded_count("personal").unwrap();
        assert_eq!(embedded, 0);
        assert_eq!(total, 4);
    }

    #[test]
    fn search_fts_finds_indexed_text() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();

        // Build an upsert with known text.
        let plan = {
            let fingerprint = fp("vault", "personal");
            let upserts = vec![upsert("notes/b.md", "rev1", "alpha bravo charlie", 1)];
            let actions = vec![FileAction::Add(obs("notes/b.md", 21, "rev1"))];
            ApplyPlan {
                root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
                roots: vec![RootApply {
                    fingerprint,
                    expected_generation: 1,
                    actions,
                    upserts,
                }],
            }
        };
        cache.apply(&plan).unwrap();

        let hits = cache
            .search_fts("personal", FtsTable::Word, "alpha", 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        // hits[0] is (chunk_id, score desc).
        let (chunk_id, score) = &hits[0];
        let rows: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE id = ?1",
                params![chunk_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
        assert!(*score > 0.0);
    }

    #[test]
    fn meta_get_and_set_roundtrip() {
        let (_dir, cache) = temp_cache();
        assert_eq!(cache.meta_get("foo").unwrap(), None);
        cache.meta_set("foo", "bar").unwrap();
        assert_eq!(cache.meta_get("foo").unwrap().as_deref(), Some("bar"));
        cache.meta_set("foo", "baz").unwrap();
        assert_eq!(cache.meta_get("foo").unwrap().as_deref(), Some("baz"));
    }

    #[test]
    fn chunks_returns_known_ids() {
        let (_dir, cache) = temp_cache();
        cache
            .apply(&add_plan("vault", "personal", "notes/a.md", "rev1"))
            .unwrap();
        let id: String = cache
            .conn
            .query_row(
                "SELECT id FROM doc_chunks ORDER BY ordinal LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let rows = cache.chunks(&[&id]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].chunk_id, id);
        assert_eq!(rows[0].root_alias, "vault");
        assert_eq!(rows[0].space, "personal");
        assert_eq!(rows[0].locator, "notes/a.md");
        assert_eq!(rows[0].revision, "rev1");
        assert_eq!(rows[0].media_type, "text/markdown");
    }

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
        assert!(cache.chunk_count_for_space("s1").unwrap() > 0);
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

    /// Build a `documents.db` at v2 by hand (the recorded previous
    /// version): run the v1 + v2 scripts, pin `user_version` to 2, and
    /// seed one v2-era root + document row. The migration up-tests open
    /// this file through `open_rw` and let the ladder advance it.
    fn v2_store(dir: &Path) {
        let conn = Connection::open(dir.join("documents.db")).unwrap();
        conn.execute_batch(V1_SCHEMA_SQL).unwrap();
        conn.execute_batch(V2_SCHEMA_SQL).unwrap();
        conn.pragma_update(None, "user_version", 2i64).unwrap();
        conn.execute(
            "INSERT INTO doc_roots(alias, space, config_hash, generation, head_revision, scanned_at)
             VALUES ('vault', 'personal', 'hash-v2', 1, NULL, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO documents(id, root_alias, space, locator, revision, media_type,
                                   bytes, modified_at, indexed_at)
             VALUES ('doc-v2', 'vault', 'personal', 'notes/a.md', 'rev1',
                     'text/markdown', 12, 0, 0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn migrate_v2_to_v3_survives_rows_with_null_pdc_columns() {
        let dir = tempfile::tempdir().unwrap();
        v2_store(dir.path());

        let cache = DocumentCache::open_rw(dir.path()).unwrap();
        assert_eq!(cache.user_version().unwrap(), 3);

        // The v2 row survived; every new column reads back NULL.
        let (uuid, profile, meta, deleted): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
        ) = cache
            .conn
            .query_row(
                "SELECT pdc_document_id, pdc_body_profile, pdc_meta, pdc_deleted
                 FROM documents WHERE id = 'doc-v2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(uuid, None);
        assert_eq!(profile, None);
        assert_eq!(meta, None);
        assert_eq!(deleted, None);

        // UUID resolution misses cleanly for a legacy row / unknown uuid.
        assert!(
            cache
                .resolve_pdc("vault", "00000000-0000-4000-8000-000000000000")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn migrate_v2_to_v3_is_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        v2_store(dir.path());
        let cache = DocumentCache::open_rw(dir.path()).unwrap();
        assert_eq!(cache.user_version().unwrap(), 3);
        drop(cache);
        let again = DocumentCache::open_rw(dir.path()).unwrap();
        assert_eq!(again.user_version().unwrap(), 3);
        assert_eq!(count(&again.conn, "documents"), 1);
    }

    /// Minimal canonical-PDC payload for the tests. `deleted` drives the
    /// `pdc_deleted` trash flag; the rest of the metadata exercises the
    /// JSON round trip through `documents.pdc_meta`.
    fn pdc_upsert(uuid: &str, deleted: bool) -> PdcProjectionUpsert {
        PdcProjectionUpsert {
            uuid: uuid.to_owned(),
            body_profile: "pdc-djot/1".to_owned(),
            meta: PdcProjectionMeta {
                title: "Envelope Title".to_owned(),
                display_title: "Display Title".to_owned(),
                deleted,
                deleted_at: deleted.then(|| "2026-09-13T12:00:00.000Z".to_owned()),
                created: "2026-09-01T08:00:00.000Z".to_owned(),
                updated: "2026-09-13T12:00:00.000Z".to_owned(),
                links: vec![oxibrain_core::documents::PdcLinkMeta {
                    uuid: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".to_owned(),
                    block: None,
                    embed: false,
                }],
                ..PdcProjectionMeta::default()
            },
        }
    }

    #[test]
    fn pdc_upsert_round_trip_resolves_by_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let uuid = "6f9619ff-8b86-4d01-b42d-00cf4fc964ff";
        {
            let cache = DocumentCache::open_rw(dir.path()).unwrap();
            let mut u = upsert("notes/pdc.djot", "rev1", "pdc body text", 1);
            u.media_type = "application/vnd.pdc.document+djot;version=1".to_owned();
            u.pdc = Some(pdc_upsert(uuid, true));
            cache
                .apply(&ApplyPlan {
                    root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
                    roots: vec![RootApply {
                        fingerprint: fp("vault", "personal"),
                        expected_generation: 0,
                        actions: vec![FileAction::Add(obs("notes/pdc.djot", 14, "rev1"))],
                        upserts: vec![u],
                    }],
                })
                .unwrap();
        }
        // Re-open read-only: the projection is usable without the writer.
        let ro = DocumentCache::open_ro(dir.path()).unwrap();
        let (document_id, locator) = ro
            .resolve_pdc("vault", uuid)
            .unwrap()
            .expect("canonical uuid resolves");
        assert_eq!(locator, "notes/pdc.djot");
        assert_eq!(
            document_id,
            oxibrain_core::documents::document_id("vault", "notes/pdc.djot")
        );

        // Columns read back exactly as written; trash rows stay resolvable
        // (deleted:true excludes from default search, not from the index).
        let (profile, deleted, meta_json): (String, i64, String) = ro
            .conn
            .query_row(
                "SELECT pdc_body_profile, pdc_deleted, pdc_meta
                 FROM documents WHERE pdc_document_id = ?1",
                params![uuid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(profile, "pdc-djot/1");
        assert_eq!(deleted, 1);
        let meta: PdcProjectionMeta = serde_json::from_str(&meta_json).unwrap();
        assert_eq!(meta, pdc_upsert(uuid, true).meta);

        // Resolution is scoped to the root.
        assert!(ro.resolve_pdc("other-root", uuid).unwrap().is_none());
    }

    #[test]
    fn pdc_uuid_conflict_is_typed_and_root_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DocumentCache::open_rw(dir.path()).unwrap();
        let shared = "0f0e1d2c-3b4a-4c5d-8e9f-0a1b2c3d4e5f";

        // The same canonical UUID in two different roots is legitimate.
        let mut vault = upsert("notes/dup.djot", "rev1", "vault copy", 1);
        vault.pdc = Some(pdc_upsert(shared, false));
        cache
            .apply(&ApplyPlan {
                root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
                roots: vec![RootApply {
                    fingerprint: fp("vault", "personal"),
                    expected_generation: 0,
                    actions: vec![FileAction::Add(obs("notes/dup.djot", 10, "rev1"))],
                    upserts: vec![vault],
                }],
            })
            .unwrap();
        let mut annex = upsert("dup.djot", "rev1", "annex copy", 1);
        annex.pdc = Some(pdc_upsert(shared, false));
        cache
            .apply(&ApplyPlan {
                root_actions: vec![("annex".to_owned(), RootAction::KeepRoot)],
                roots: vec![RootApply {
                    fingerprint: fp("annex", "personal"),
                    expected_generation: 0,
                    actions: vec![FileAction::Add(obs("dup.djot", 10, "rev1"))],
                    upserts: vec![annex],
                }],
            })
            .unwrap();

        // Same UUID, same root, different locator → typed `Invalid` naming
        // both locators; the whole apply rolls back.
        let mut clash = upsert("notes/other.djot", "rev1", "clash", 1);
        clash.pdc = Some(pdc_upsert(shared, true));
        let err = cache
            .apply(&ApplyPlan {
                root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
                roots: vec![RootApply {
                    fingerprint: fp("vault", "personal"),
                    expected_generation: 1,
                    actions: vec![FileAction::Add(obs("notes/other.djot", 6, "rev1"))],
                    upserts: vec![clash],
                }],
            })
            .unwrap_err();
        match err {
            BrainError::Invalid(msg) => {
                assert!(msg.contains(shared), "names the uuid: {msg}");
                assert!(
                    msg.contains("notes/dup.djot"),
                    "names the existing locator: {msg}"
                );
                assert!(
                    msg.contains("notes/other.djot"),
                    "names the conflicting locator: {msg}"
                );
            }
            other => panic!("expected BrainError::Invalid, got {other:?}"),
        }
        assert_eq!(count(&cache.conn, "documents"), 2);
        // Resolve still lands on the original locator for that uuid.
        let (_, locator) = cache.resolve_pdc("vault", shared).unwrap().unwrap();
        assert_eq!(locator, "notes/dup.djot");
    }
}
