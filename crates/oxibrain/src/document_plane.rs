//! Document plane sequencer (`crates/oxibrain/src/document_plane.rs`).
//!
//! Implements Task 6 of the daemonless two-plane plan. This module sequences
//! the document-cache plane (the `documents.db` cache, gix-aware roots, and
//! the RRF-ranked search side). It is **strictly a sequencer**:
//!
//! - All SQL lives in [`oxibrain_store::documents::DocumentCache`].
//! - All pure reconcile / identity decisions live in
//!   [`oxibrain_core::documents`].
//! - Filesystem and gix I/O live in [`oxibrain_connectors`]`::{scan, git_docs,
//!   decode, documents_config}`.
//!
//! P9 (separating decisions, storage, and sequencing) is preserved: this file
//! owns the orchestration only. The rules below are enforced by review:
//!
//! - The LLM, embedder, and tokenizer are never invoked inside a `documents.db`
//!   or `brain.db` write transaction.
//! - `Brain` is **handle-free**: there is no `StoreHandle`. Every method opens
//!   its store connections on demand and drops them before returning.
//! - One write op = one locked open with bounded retry.
//!
//! ## Index / Search flow (Task 6 §6.7)
//!
//! 1. `index_documents` reads `documents.toml`, walks configured roots,
//!    calls `GitDocumentReader::open` (None for plain roots), calls the pure
//!    planners in `core::documents`, decodes Add/Replace candidates, splits
//!    them with the existing core chunking policy, and stores the result in
//!    one atomic `DocumentCache::apply` call. `Busy` from the CAS path ⇒ one
//!    rescan + retry, then the error surfaces.
//! 2. `search` (documents plane) reconciles once, runs `search_fts` for both
//!    tokenizers (and `knn` when an embedder is configured with a matching
//!    model identity), fuses with RRF (k=60), then materializes the top hits.
//!    Per chunk: re-read the file, decode, slice the span, verify the
//!    `revision` still matches. Mismatch ⇒ the hit is dropped.
//! 3. `document_history` is a thin shim over `GitDocumentReader::history`
//!    (plan §6.7 "git-only history").
//! 4. `embed_pending_documents` looks up `pending_vector_chunks` per space,
//!    embeds outside any transaction, and `upsert_vectors`. A model-identity
//!    change (`doc_embed_model_id` mismatch) → `clear_vectors` first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use oxibrain_connectors::documents_config::{
    DEFAULT_MAX_FILE_BYTES, DocumentsConfig, RootEntry, UpsertOutcome, default_exclude,
    default_include,
};
use oxibrain_connectors::pdc::{BodyProfile, HtmlClassification};
use oxibrain_connectors::scan::{canonicalize_root, scan_root};
use oxibrain_connectors::{
    DECODER_VERSION, GitDocumentReader, MediaType, PdcDiagnostic, PdcDiagnosticCode, PdcDocument,
    classify_html_transport, decode, parse_djot_document, parse_html_document,
};
use oxibrain_core::chunking::{ChunkPolicy, render_context_prefix, split_into_chunks};
use oxibrain_core::documents::{
    CachedFile, CachedRootMeta, FileAction as CoreFileAction, FileObservation, PdcLinkMeta,
    PdcProjectionMeta, RootAction, RootFingerprint,
};
use oxibrain_core::retrieval::{Query as CoreQuery, QueryMode as CoreQueryMode, SearchPlane};
use oxibrain_ports::{BrainError, EmbeddingPort, Timestamp};
use oxibrain_store::documents::{
    ApplyPlan, CachedChunk, ChunkUpsert as StoreChunkUpsert, DocumentCache, DocumentUpsert,
    DocumentsLock, FtsTable, PdcProjectionUpsert, RootApply as StoreRootApply,
};
use oxibrain_store::ledger;

use crate::Brain;

// ─── Public surface ─────────────────────────────────────────────────────────

/// Verbatim retrieved text, typed as untrusted at the boundary (spec
/// `agent-first-cli-v1` §4): oxibrain feeds untrusted documents to agents,
/// so a hit is never a bare string an agent might mistake for instructions.
/// Defense is representational, never detective — no language-specific
/// scanning (P11).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UntrustedContent {
    /// Always `"untrusted_content"`.
    pub kind: String,
    pub text: String,
    pub provenance: ContentProvenance,
}

/// Where an `UntrustedContent` value came from. `trust` is `"unverified"`
/// until per-root trust evaluation exists — which is the truth today.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContentProvenance {
    /// Stable URI (`doc://<alias>/<locator>?rev=<revision>`).
    #[serde(rename = "ref")]
    pub reference: String,
    pub trust: String,
}

/// A single document plane hit (§7.2). Score is the post-RRF value inside
/// the documents plane; it is never compared against memory-plane scores.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentHit {
    pub document_id: String,
    pub root: String,
    pub locator: String,
    pub revision: String,
    pub ordinal: u32,
    pub text: UntrustedContent,
    pub modified_at: Timestamp,
    pub score: f64,
}

/// One diagnosable document-plane condition from a reconcile pass
/// (`pdc-adoption-v1` "Diagnostics"): a PDC parse/validation failure, a
/// duplicate canonical UUID claim, an unresolved link, or a managed-asset
/// problem. Diagnostics never abort the pass — unrelated valid documents
/// still index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticReport {
    /// Root alias the document belongs to.
    pub alias: String,
    /// Root-relative locator of the document.
    pub locator: String,
    /// Contract diagnostic code in `as_str` spelling
    /// (`invalid_transport`, `duplicate_document_id`, …).
    pub code: String,
    /// Human-readable explanation; may aggregate several targets.
    pub reason: String,
}

/// Summary that `index_documents` returns so callers can report freshness
/// to operators (`doctor`, MCP `stats`, MCP `search` freshness field).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentFreshness {
    /// Aliases that were reconciled in this pass (root row bumped).
    pub reconciled_roots: Vec<String>,
    /// Aliases that were skipped — `(alias, reason)`. Missing roots land
    /// here; refuse-to-walk roots (permission errors, etc.) land here too.
    pub skipped_roots: Vec<(String, String)>,
    /// Files that the walker deliberately ignored (oversize, unreadable,
    /// excluded by include/exclude, gix-ignored). Surfaced for `doctor`.
    pub skipped_files: usize,
    /// Locators that flipped twice between two reads (a real materialization
    /// race — see spec §12 invariant 12).
    pub stale_after_retry: Vec<String>,
    /// PDC + legacy decode diagnostics collected across all reconciled
    /// roots in this pass, in root order then locator order.
    pub diagnostics: Vec<DiagnosticReport>,
    /// Documents that classified as visible legacy HTML and went through
    /// the legacy adapter (`legacy_html` outcome).
    pub legacy_html: u32,
    /// `embedded / total` when the dense channel ran; `None` otherwise.
    pub dense_coverage: Option<f64>,
}

/// Render diagnostic reports as grouped operator-facing lines (`oxibrain
/// index` / `oxibrain doctor`): one header per code (sorted), then one line
/// per diagnostic, capped at `max_lines` with an omission note.
pub fn format_diagnostic_lines(diags: &[DiagnosticReport], max_lines: usize) -> Vec<String> {
    let mut grouped: std::collections::BTreeMap<&str, Vec<&DiagnosticReport>> = Default::default();
    for d in diags {
        grouped.entry(d.code.as_str()).or_default().push(d);
    }
    let mut lines: Vec<String> = Vec::new();
    for (code, group) in &grouped {
        lines.push(format!("{code}:"));
        for d in group {
            lines.push(format!("  {}/{}: {}", d.alias, d.locator, d.reason));
        }
    }
    if lines.len() > max_lines {
        let keep = max_lines.saturating_sub(1);
        let omitted = lines.len() - keep;
        lines.truncate(keep);
        lines.push(format!("… {omitted} more diagnostic line(s) omitted"));
    }
    lines
}

/// Options accepted by `Brain::index_documents`.
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// When true, embed any pending chunk after the apply pass.
    pub embed: bool,
    /// Cap on the embed pass (number of chunks). `None` = consume all.
    pub budget: Option<usize>,
}

/// `Brain::search()`'s response envelope. Memory and document lists are
/// always returned even when a plane is empty — callers do not have to
/// branch on "did this plane run".
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SearchResponse {
    pub memory: Vec<oxibrain_store::query::SearchResult>,
    pub documents: Vec<DocumentHit>,
    pub freshness: DocumentFreshness,
}

/// `Brain::pending_extraction_stats()`'s response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingStats {
    pub count: u64,
    pub oldest_seq: Option<u64>,
}

/// Typed registration request for one document root — the surface other
/// apps use to declare a vault instead of editing `documents.toml`
/// themselves (unified-home ownership rule: only oxibrain writes its own
/// config). `None` rules fall back to the `documents.toml` defaults, so a
/// registration with no include/exclude/limit lands exactly where a
/// hand-written entry with omitted fields would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRootSpec {
    /// Logical space the root's documents belong to.
    pub space: String,
    /// Unique alias inside `documents.toml` — the upsert key.
    pub alias: String,
    /// Filesystem path of the root (absolute recommended; `~` expands at load).
    pub path: PathBuf,
    /// Include globs; `None` = connector defaults.
    pub include: Option<Vec<String>>,
    /// Exclude globs; `None` = connector defaults.
    pub exclude: Option<Vec<String>>,
    /// Per-file byte cap; `None` = connector default.
    pub max_file_bytes: Option<u64>,
}

/// Idempotent outcome of a root registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterRootOutcome {
    /// No entry existed for the alias; it was appended.
    Added,
    /// The alias existed with different rules; it was replaced in place.
    Replaced,
    /// An identical entry already existed; nothing was written.
    Unchanged,
}

/// What a registration did plus the effective entry now on disk.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RegistrationResult {
    pub outcome: RegisterRootOutcome,
    pub root: RootEntry,
}

impl Brain {
    // ─── search / recall ───────────────────────────────────────────────────

    /// Planes-aware hybrid search (§7.2). Defaults: both planes.
    ///
    /// When `Documents` is requested the sequencer reconciles the document
    /// cache, runs the configured channels, fuses with RRF, materializes the
    /// top hits, and returns them in `SearchResponse.documents`. The memory
    /// plane keeps its existing behavior: legacy `document` /
    /// `document_revision` episodes are excluded at the FTS fetch so they
    /// don't pollute `/ask`.
    pub async fn search(&self, q: CoreQuery) -> Result<SearchResponse, BrainError> {
        // Step 1: documents plane (reconcile + search + materialize).
        let mut freshness = DocumentFreshness::default();
        let mut documents: Vec<DocumentHit> = Vec::new();
        let dense_channel_ran;
        if q.planes.contains(&SearchPlane::Documents) {
            freshness = self.index_documents_in_place().await?;
            // The documents cache is scoped by the root's `space` NAME from
            // documents.toml, while `Query.space` conventionally carries the
            // content-derived id (MCP resolves via `ensure_space`). Translate
            // id → name once; an unknown string is used verbatim so callers
            // that already pass names keep working.
            let mut doc_q = q.clone();
            doc_q.space = self.resolve_space_name(&q.space).await?;
            documents = self.search_documents(&doc_q).await?;
            dense_channel_ran = self.embedder.is_some()
                && matches!(q.mode, CoreQueryMode::Hybrid | CoreQueryMode::Dense);
            if dense_channel_ran {
                freshness.dense_coverage = self.dense_coverage(&doc_q.space).await.unwrap_or(None);
            }
        }

        // Step 2: memory plane (legacy exclusion + the existing hybrid path).
        let mut memory: Vec<oxibrain_store::query::SearchResult> = Vec::new();
        if q.planes.contains(&SearchPlane::Memory) {
            memory = self.search_memory(&q).await?;
        }

        Ok(SearchResponse {
            memory,
            documents,
            freshness,
        })
    }

    /// Resolve a space identifier to the NAME the documents plane stores.
    /// `Query.space` may be a content-derived id (MCP path) or already a
    /// name (direct facade callers); a string that matches no space id is
    /// returned unchanged.
    async fn resolve_space_name(&self, space: &str) -> Result<String, BrainError> {
        let target = space.to_string();
        let fallback = target.clone();
        let found = self
            .read(move |conn| {
                Ok(ledger::list_spaces(conn)?
                    .into_iter()
                    .find(|s| s.id == target)
                    .map(|s| s.name))
            })
            .await?;
        Ok(found.unwrap_or(fallback))
    }

    /// Recall with a Documents layer: reconcile once, fetch the top document
    /// excerpts for the query, and let `core::pack` place them between
    /// QueryNeighborhood and RecentEpisodes (the layer ordering and reserve
    /// floor live in core — this method only attaches the excerpts).
    pub async fn recall(
        &self,
        space: &str,
        query: &str,
        budget: usize,
        hints: Option<&oxibrain_store::context::RecallHints>,
    ) -> Result<oxibrain_core::context::ContextResult, BrainError> {
        // Reconcile once so the document cache has fresh state, then fetch
        // a small Documents layer. Best-effort: a read-only brain skips the
        // reconcile and packs whatever the cache already holds.
        if !self.config.read_only {
            let _ = self.index_documents_in_place().await?;
        }
        let doc_space = self.resolve_space_name(space).await?;
        let doc_hits: Vec<DocumentHit> = {
            let q = CoreQuery {
                text: query.to_string(),
                mode: CoreQueryMode::Lexical,
                space: doc_space,
                as_of: None,
                limit: 5,
                min_confidence: 0.0,
                planes: [SearchPlane::Documents].into_iter().collect(),
            };
            self.search_documents(&q).await?
        };
        let documents: Vec<oxibrain_core::pack::DocumentExcerpt> = doc_hits
            .iter()
            .map(|h| oxibrain_core::pack::DocumentExcerpt {
                uri: build_doc_uri(&h.root, &h.locator, &h.revision),
                // Internal consumer (context packing) unwraps — the
                // untrusted typing applies to agent-facing boundaries.
                text: h.text.text.clone(),
                score: h.score as f32,
            })
            .collect();

        let tokenizer = self.tokenizer.clone();
        let query_owned = query.to_string();
        let space_owned = space.to_string();
        let hints_owned = hints.cloned();
        self.read(move |conn| {
            let mut input = oxibrain_store::context::build_context_input(
                conn,
                &space_owned,
                &query_owned,
                hints_owned.as_ref(),
            )?;
            input.documents = documents;
            let policy = oxibrain_core::pack::PackPolicy::for_budget(budget);
            Ok(oxibrain_core::pack::pack(
                &input,
                &oxibrain_core::context::ContextBudget { max_tokens: budget },
                &policy,
                tokenizer.as_ref(),
            ))
        })
        .await
    }

    // ─── indexing ──────────────────────────────────────────────────────────

    /// Index the configured documents plane now. `Busy` from the apply CAS ⇒
    /// one rescan + retry; a second failure surfaces the error.
    pub async fn index_documents(
        &self,
        opts: IndexOptions,
    ) -> Result<DocumentFreshness, BrainError> {
        let mut freshness = self.index_documents_in_place().await?;
        if opts.embed {
            let budget = opts.budget.unwrap_or(usize::MAX);
            self.embed_pending_documents(budget).await?;
            // The vector channel just ran — report coverage across every
            // configured space (None when there is nothing to embed).
            freshness.dense_coverage = self.dense_coverage_all().await?;
        }
        Ok(freshness)
    }

    /// Embed pending chunk vectors up to `budget` chunks (all spaces declared
    /// in `documents.toml`). The embedder is required — when absent we return
    /// a `Config` error so callers can surface it loudly. On a model-identity
    /// mismatch (`doc_embed_model_id` differs from the current embedder's
    /// identity) we `clear_vectors` first so stale vectors never answer a
    /// dense query (spec §6 / invariants §11).
    pub async fn embed_pending_documents(&self, budget: usize) -> Result<usize, BrainError> {
        let embedder = self
            .embedder
            .as_ref()
            .ok_or_else(|| BrainError::Config("no embedding port configured".into()))?
            .clone();
        let identity = embedder_identity(&*embedder);
        let dir = self.config.dir.clone();

        // Spaces come from the configured roots — `pending_vector_chunks`
        // is space-scoped in the store.
        let spaces: Vec<String> = configured_spaces(&dir)?;
        if spaces.is_empty() {
            return Ok(0);
        }

        // Phase 1 [locked open]: identity check + pending fetch.
        let pending: Vec<(String, String)> = {
            let dir = dir.clone();
            let identity = identity.clone();
            blocking(move || {
                let cache = open_cache_rw_with_retry(&dir)?;
                if cache.meta_get("doc_embed_model_id")? != Some(identity.clone()) {
                    cache.clear_vectors()?;
                    cache.meta_set("doc_embed_model_id", &identity)?;
                }
                let mut out: Vec<(String, String)> = Vec::new();
                for space in &spaces {
                    if out.len() >= budget {
                        break;
                    }
                    out.extend(cache.pending_vector_chunks(space, budget - out.len())?);
                }
                Ok(out)
            })
            .await?
        };

        if pending.is_empty() {
            return Ok(0);
        }

        // Phase 2: embed outside any DB transaction (P9 / invariant §3).
        let texts: Vec<String> = pending.iter().map(|(_, t)| t.clone()).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let vectors = embedder
            .embed(&refs)
            .map_err(|e| BrainError::Config(format!("embedding failed: {e}")))?;
        let rows: Vec<(String, Vec<f32>)> = pending
            .into_iter()
            .zip(vectors)
            .map(|((id, _), v)| (id, v))
            .collect();
        let count = rows.len();

        // Phase 3 [locked open]: upsert + identity record.
        blocking(move || {
            let cache = open_cache_rw_with_retry(&dir)?;
            cache.upsert_vectors(&rows)?;
            cache.meta_set("doc_embed_model_id", &identity)?;
            Ok(())
        })
        .await?;

        Ok(count)
    }

    /// Native JSON-RPC method: commit history for one tracked locator.
    /// Returns `BrainError::Invalid("pinning requires a git root")` for a
    /// plain root (we have no history to surface).
    pub async fn document_history(
        &self,
        space: &str,
        alias: &str,
        locator: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_connectors::DocumentRevision>, BrainError> {
        let _ = space; // reserved: future per-space history filtering
        let canonical_root = self
            .locator_to_root_path(alias)
            .ok_or_else(|| BrainError::Invalid(format!("unknown alias {alias}")))?;
        let reader = GitDocumentReader::open(&canonical_root)?
            .ok_or_else(|| BrainError::Invalid("pinning requires a git root".into()))?;
        reader.history(alias, locator, limit)
    }

    // ─── memory-plane backlog stats ────────────────────────────────────────

    /// Memory-plane backlog stats (the queue-less `extract_uncached` view,
    /// across all spaces): how many primary non-document episodes the drain
    /// would attempt now — no extraction row for the current extractor and
    /// no failure for it within [`FAILURE_RETRY_COOLDOWN`] — and the oldest
    /// backlog seq. Must mirror `uncached_memory_episodes` exactly so
    /// schedulers see the retry schedule, not a number that never moves.
    pub async fn pending_extraction_stats(&self) -> Result<PendingStats, BrainError> {
        const FILTER: &str = "WHERE e.kind = 'primary'
                         AND e.source_kind NOT IN ('document', 'document_revision')
                         AND e.redacted_at IS NULL
                         AND NOT EXISTS (SELECT 1 FROM extractions x
                                         WHERE x.episode_id = e.id
                                           AND x.extractor_id = ?1)
                         AND NOT EXISTS (SELECT 1 FROM extraction_failures f
                                         WHERE f.episode_id = e.id
                                           AND f.extractor_id = ?1
                                           AND f.created_at >= ?2)";
        let extractor_id = crate::extraction::default_extractor_config().id();
        let cutoff = Timestamp::from_millis(
            self.clock.now().millis()
                - crate::extraction::FAILURE_RETRY_COOLDOWN.as_millis() as i64,
        );
        self.read(move |conn| {
            let count: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM episodes e {FILTER}"),
                    rusqlite::params![extractor_id, cutoff.millis()],
                    |r| r.get(0),
                )
                .map_err(|e| BrainError::Storage(format!("pending count: {e}")))?;
            let oldest_seq: Option<i64> = conn
                .query_row(
                    &format!("SELECT MIN(e.seq) FROM episodes e {FILTER}"),
                    rusqlite::params![extractor_id, cutoff.millis()],
                    |r| r.get(0),
                )
                .ok();
            Ok(PendingStats {
                count: count.max(0) as u64,
                oldest_seq: oldest_seq.map(|s| s.max(0) as u64),
            })
        })
        .await
    }

    /// Cached document inventory for the operator surfaces (`doctor`,
    /// `stats`): `(configured roots, cached files)`. A missing `documents.db`
    /// reports zero cached files rather than an error — a fresh brain that
    /// has never reconciled is healthy, not broken.
    pub async fn document_counts(&self) -> Result<(usize, usize), BrainError> {
        let dir = self.config.dir.clone();
        blocking(move || {
            let roots = load_documents_config(&dir)?.roots.len();
            let files = match DocumentCache::open_ro(&dir) {
                Ok(cache) => {
                    let mut total = 0usize;
                    for meta in cache.list_roots()? {
                        total += cache.root_manifest(&meta.alias)?.len();
                    }
                    total
                }
                Err(BrainError::NotFound(_)) => 0,
                Err(e) => return Err(e),
            };
            Ok((roots, files))
        })
        .await
    }

    /// `doc://` refs still in the ledger whose alias is no longer declared in
    /// `documents.toml` — the dangling-reference report for `doctor`. Such
    /// episodes are provenance-only legacy rows (spec §11.3); the operator
    /// decides whether to re-add the alias or redact them.
    pub async fn dangling_document_refs(&self) -> Result<Vec<String>, BrainError> {
        let dir = self.config.dir.clone();
        let cfg = blocking(move || load_documents_config(&dir)).await?;
        self.read(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT source_ref FROM episodes
                     WHERE source_kind IN ('document', 'document_revision')
                       AND source_ref LIKE 'doc://%'
                       AND redacted_at IS NULL",
                )
                .map_err(|e| BrainError::Storage(format!("dangling refs prepare: {e}")))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| BrainError::Storage(format!("dangling refs query: {e}")))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| BrainError::Storage(format!("dangling refs row: {e}")))?;
            Ok(rows
                .into_iter()
                .filter(|uri| {
                    let alias = uri
                        .strip_prefix("doc://")
                        .and_then(|rest| rest.split('/').next())
                        .unwrap_or("");
                    !alias.is_empty() && cfg.root(alias).is_none()
                })
                .collect())
        })
        .await
    }

    // ─── reconcile sequencing ──────────────────────────────────────────────

    /// Reconcile the documents plane (one atomic apply) and return the
    /// freshness outcome. `Busy` ⇒ one rescan + retry; a second `Busy`
    /// surfaces. Read-only brains skip the reconcile entirely and report
    /// what the cache already holds.
    async fn index_documents_in_place(&self) -> Result<DocumentFreshness, BrainError> {
        if self.config.read_only {
            return Ok(DocumentFreshness::default());
        }
        match self.reconcile_once().await {
            Ok(o) => Ok(o),
            Err(BrainError::Busy(_)) => {
                // One retry: rescan from scratch so the CAS sees the new
                // generation, then return the second outcome plainly.
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                self.reconcile_once().await
            }
            Err(e) => Err(e),
        }
    }

    /// One full reconcile + apply pass: load config, diff root sets, walk
    /// each kept/reset root, decode Add/Replace payloads, apply atomically.
    async fn reconcile_once(&self) -> Result<DocumentFreshness, BrainError> {
        let dir = self.config.dir.clone();
        let cfg = load_documents_config(&dir)?;

        // Fingerprint every configured root. A root that cannot be
        // canonicalized (missing on disk, permission denied) is skipped —
        // its cached rows survive so the alias coming back is cheap.
        let mut configured: Vec<(RootEntry, RootFingerprint)> = Vec::new();
        let mut skipped_roots: Vec<(String, String)> = Vec::new();
        for entry in cfg.roots {
            match fingerprint_from_entry(&entry) {
                Ok(fp) => configured.push((entry, fp)),
                Err(e) => skipped_roots.push((entry.alias, e.to_string())),
            }
        }

        // Cached roots from documents.db; a missing cache is the zero state.
        let cached_roots: Vec<CachedRootMeta> = {
            let dir = dir.clone();
            blocking(move || {
                Ok(match DocumentCache::open_ro(&dir) {
                    Ok(cache) => cache.list_roots()?,
                    Err(BrainError::NotFound(_)) => Vec::new(),
                    Err(e) => return Err(e),
                })
            })
            .await?
        };

        // Nothing configured and nothing cached: skip the advisory lock
        // entirely — pure-memory brains never touch documents.db.
        if configured.is_empty() && cached_roots.is_empty() {
            return Ok(DocumentFreshness {
                skipped_roots,
                ..DocumentFreshness::default()
            });
        }

        // Pure root-set diff (decisions in core).
        let fingerprints: Vec<RootFingerprint> =
            configured.iter().map(|(_, fp)| fp.clone()).collect();
        let root_actions = oxibrain_core::documents::diff_roots(&fingerprints, &cached_roots);

        // Per kept/reset root: scan → plan → payload.
        let mut roots_apply: Vec<StoreRootApply> = Vec::new();
        let mut reconciled_roots: Vec<String> = Vec::new();
        let mut skipped_files_total: usize = 0;
        let mut stale_after_retry: Vec<String> = Vec::new();
        let mut diagnostics: Vec<DiagnosticReport> = Vec::new();
        let mut legacy_html = 0u32;

        for (entry, _) in &configured {
            let alias = entry.alias.clone();
            let action = root_actions
                .iter()
                .find(|(a, _)| a == &alias)
                .map(|(_, act)| act.clone())
                .unwrap_or(RootAction::KeepRoot);
            match self
                .index_one_root(entry, action, &mut stale_after_retry)
                .await
            {
                Ok(outcome) => {
                    skipped_files_total += outcome.skipped_files;
                    reconciled_roots.push(outcome.alias);
                    roots_apply.push(outcome.root_apply);
                    diagnostics.extend(outcome.diagnostics);
                    legacy_html += outcome.legacy_html;
                }
                Err(e) => skipped_roots.push((alias, e.to_string())),
            }
        }

        // One atomic apply. Lock acquisition retries with backoff inside
        // `open_cache_rw_with_retry`; a CAS mismatch (Busy) propagates so
        // `index_documents_in_place` can rescan once.
        let plan = ApplyPlan {
            root_actions,
            roots: roots_apply,
        };
        blocking(move || {
            let cache = open_cache_rw_with_retry(&dir)?;
            cache.apply(&plan)
        })
        .await?;
        Ok(DocumentFreshness {
            reconciled_roots,
            skipped_roots,
            skipped_files: skipped_files_total,
            stale_after_retry,
            diagnostics,
            legacy_html,
            dense_coverage: None,
        })
    }

    /// Walk, plan, and decode one root. `stale` collects locators that
    /// flipped twice between the scan stat and the payload read.
    async fn index_one_root(
        &self,
        entry: &RootEntry,
        action: RootAction,
        stale: &mut Vec<String>,
    ) -> Result<RootIndexOutcome, BrainError> {
        // Canonical path can fail when the root is missing on disk; the
        // facade routes that into `skipped_roots` (caller).
        let canonical = canonicalize_root(&entry.path)?;
        let scan = scan_root(entry)?;

        // Cached manifest (read-only open; missing cache = empty manifest).
        let cached_manifest: Vec<CachedFile> = {
            let dir = self.config.dir.clone();
            let alias = entry.alias.clone();
            blocking(move || {
                Ok(match DocumentCache::open_ro(&dir) {
                    Ok(cache) => cache.root_manifest(&alias)?,
                    Err(BrainError::NotFound(_)) => Vec::new(),
                    Err(e) => return Err(e),
                })
            })
            .await?
        };

        // Open the gix reader (None for plain roots). Even plain dirs
        // return `Ok(None)`, so `is_ignored` is only consulted when Some.
        let git_reader = GitDocumentReader::open(&canonical)?;
        let snapshot = match &git_reader {
            Some(r) => Some(r.snapshot()?),
            None => None,
        };

        let mut observed: Vec<FileObservation> = Vec::with_capacity(scan.observations.len());
        let mut skipped_files = scan.skipped.len();
        for mut o in scan.observations {
            if let Some(reader) = &git_reader
                && let Some(snap) = &snapshot
            {
                if reader.is_ignored(&o.locator)? {
                    skipped_files += 1;
                    continue;
                }
                // Revision: reuse the cached `git:` revision when the
                // stat is unchanged (bytes are read ONLY for changed
                // candidates — clean tracked files keep their hint).
                let cached_rev = cached_manifest
                    .iter()
                    .find(|c| c.locator == o.locator)
                    .filter(|c| c.bytes == o.bytes && c.modified_ns == o.modified_ns)
                    .map(|c| c.revision.clone());
                let rev = match cached_rev {
                    Some(r) if r.starts_with("git:") => r,
                    _ => {
                        let bytes = read_worktree_bytes(&canonical, &o.locator)?;
                        reader.current_revision(snap, &o.locator, &bytes)?
                    }
                };
                o.revision_hint = Some(rev);
            }
            observed.push(o);
        }

        // Pure per-root plan (decisions in core).
        let mut actions: Vec<CoreFileAction> =
            oxibrain_core::documents::plan_reconcile(&cached_manifest, &observed);

        // Decode Add/Replace payloads. A file that vanished between scan
        // and read is a skip, not a root failure; a rejected PDC document
        // is a recorded diagnostic. Every materialization failure converts
        // its action to `Skip` so apply never sees an Add/Replace without
        // a matching upsert.
        let mut upserts: Vec<DocumentUpsert> = Vec::new();
        // Parallel to `upserts`: index into `actions` of the Add/Replace
        // each upsert was materialized from.
        let mut action_of: Vec<usize> = Vec::new();
        let mut diagnostics: Vec<DiagnosticReport> = Vec::new();
        let mut legacy_html = 0u32;
        for (idx, slot) in actions.iter_mut().enumerate() {
            let obs = match &*slot {
                CoreFileAction::Add(o) | CoreFileAction::Replace(o) => o.clone(),
                _ => continue,
            };
            match self.materialize_upsert(&obs, &canonical, stale).await {
                Ok(MaterializedUpsert::Pdc(upsert)) => {
                    upserts.push(upsert);
                    action_of.push(idx);
                }
                Ok(MaterializedUpsert::Legacy(upsert)) => {
                    legacy_html += 1;
                    upserts.push(upsert);
                    action_of.push(idx);
                }
                Err(MaterializeError::Pdc(d)) => {
                    let reason = diagnostic_reason(&d);
                    diagnostics.push(DiagnosticReport {
                        alias: entry.alias.clone(),
                        locator: obs.locator.clone(),
                        code: d.code.as_str().to_string(),
                        reason: reason.clone(),
                    });
                    skipped_files += 1;
                    *slot = CoreFileAction::Skip {
                        locator: obs.locator.clone(),
                        reason: format!("{}: {reason}", d.code.as_str()),
                    };
                }
                Err(MaterializeError::Io(e)) => {
                    skipped_files += 1;
                    *slot = CoreFileAction::Skip {
                        locator: obs.locator.clone(),
                        reason: e.to_string(),
                    };
                }
            }
        }

        // Root-level canonical pass (pdc-adoption-v1): resolve duplicate
        // UUID claims first — they shrink the surviving uuid → locator map —
        // then report unresolved links and verify managed assets for every
        // surviving canonical document.
        let uuid_map = resolve_duplicate_pdc_ids(
            &entry.alias,
            &mut actions,
            &mut upserts,
            &mut action_of,
            &mut diagnostics,
        );
        for upsert in &upserts {
            let Some(pdc) = &upsert.pdc else { continue };
            // Unresolved `pdc://document/<uuid>` links: target UUID absent
            // from THIS root's pass. One informational diagnostic per
            // document with the missing targets aggregated into the reason.
            let missing: std::collections::BTreeSet<&str> = pdc
                .meta
                .links
                .iter()
                .map(|l| l.uuid.as_str())
                .filter(|uuid| !uuid_map.contains_key(*uuid))
                .collect();
            if !missing.is_empty() {
                diagnostics.push(unresolved_link_report(
                    &entry.alias,
                    &upsert.locator,
                    &missing,
                ));
            }
            // Managed assets: each referenced digest must exist under
            // `<root>/.pdc/assets/sha256/<2hex>/<digest>` and hash to
            // itself. Deduped per document; verified by streaming read.
            for digest in pdc
                .meta
                .assets
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
            {
                if let Err((code, reason)) = verify_managed_asset(&canonical, digest) {
                    diagnostics.push(DiagnosticReport {
                        alias: entry.alias.clone(),
                        locator: upsert.locator.clone(),
                        code: code.as_str().to_string(),
                        reason,
                    });
                }
            }
        }

        // CAS generation: KeepRoot must match the cached row; a reset root
        // was cascade-deleted in apply Phase 1, so its expected value is 0.
        let expected_generation = match action {
            RootAction::KeepRoot => {
                let dir = self.config.dir.clone();
                let alias = entry.alias.clone();
                blocking(move || {
                    Ok(match DocumentCache::open_ro(&dir) {
                        Ok(cache) => cache.generation(&alias)?,
                        Err(BrainError::NotFound(_)) => 0,
                        Err(e) => return Err(e),
                    })
                })
                .await?
            }
            RootAction::ResetRoot | RootAction::RemoveRoot => 0,
        };

        Ok(RootIndexOutcome {
            alias: entry.alias.clone(),
            skipped_files,
            root_apply: StoreRootApply {
                fingerprint: fingerprint_from_entry(entry)?,
                expected_generation,
                actions,
                upserts,
            },
            diagnostics,
            legacy_html,
        })
    }

    /// Decode one Add/Replace action into chunks + manifest payload,
    /// classification-aware (pdc-adoption-v1):
    /// - `.djot` and canonical-PDC `.html` parse through the PDC connector;
    ///   the upsert carries the contract media type and the projection
    ///   payload.
    /// - Legacy `.html` (and every other extension) keep the legacy decoder
    ///   path; legacy HTML is counted in the `legacy_html` report.
    /// - A PDC connector rejection surfaces as [`MaterializeError::Pdc`]
    ///   (reportable diagnostic, no upsert).
    ///
    /// Stability check (invariant §3): stat before/after the read; on a
    /// mismatch retry the read once; a second mismatch flags the locator
    /// in `stale` (the payload still lands — the next pass re-reconciles).
    async fn materialize_upsert(
        &self,
        obs: &FileObservation,
        canonical_root: &Path,
        stale: &mut Vec<String>,
    ) -> Result<MaterializedUpsert, MaterializeError> {
        let path = canonical_root.join(&obs.locator);

        let mut bytes = std::fs::read(&path).map_err(|e| {
            MaterializeError::Io(BrainError::Storage(format!("read {}: {e}", path.display())))
        })?;
        if !stat_matches(&path, obs) {
            bytes = std::fs::read(&path).map_err(|e| {
                MaterializeError::Io(BrainError::Storage(format!(
                    "re-read {}: {e}",
                    path.display()
                )))
            })?;
            if !stat_matches(&path, obs) {
                stale.push(obs.locator.clone());
            }
        }

        let media_type = media_type_of(&path);
        match media_type {
            MediaType::Djot => {
                let doc = parse_djot_document(&locator_stem(&obs.locator), &bytes)
                    .map_err(MaterializeError::Pdc)?;
                Ok(MaterializedUpsert::Pdc(build_upsert(
                    obs,
                    &bytes,
                    DecodedSource::Pdc(Box::new(doc)),
                )))
            }
            MediaType::Html => match classify_html_transport(&bytes) {
                HtmlClassification::Pdc => {
                    let doc = parse_html_document(&locator_stem(&obs.locator), &bytes)
                        .map_err(MaterializeError::Pdc)?;
                    Ok(MaterializedUpsert::Pdc(build_upsert(
                        obs,
                        &bytes,
                        DecodedSource::Pdc(Box::new(doc)),
                    )))
                }
                // Visible legacy HTML stays on the legacy adapter and is
                // reported as `legacy_html` — never an error.
                HtmlClassification::Legacy => {
                    let decoded = decode(MediaType::Html, &bytes).map_err(MaterializeError::Io)?;
                    Ok(MaterializedUpsert::Legacy(build_upsert(
                        obs,
                        &bytes,
                        DecodedSource::Legacy {
                            media_type: MediaType::Html.as_str().to_string(),
                            text: decoded.text,
                        },
                    )))
                }
            },
            _ => {
                let decoded = decode(media_type, &bytes).map_err(MaterializeError::Io)?;
                Ok(MaterializedUpsert::Legacy(build_upsert(
                    obs,
                    &bytes,
                    DecodedSource::Legacy {
                        media_type: media_type.as_str().to_string(),
                        text: decoded.text,
                    },
                )))
            }
        }
    }

    // ─── documents search ──────────────────────────────────────────────────

    /// Search the documents plane: both FTS channels (+ knn when the mode
    /// wants dense and an embedder is configured), RRF-fused, then
    /// materialized against the current files.
    async fn search_documents(&self, q: &CoreQuery) -> Result<Vec<DocumentHit>, BrainError> {
        let dir = self.config.dir.clone();
        // Never indexed (or first run before any index op): zero state.
        if !dir.join("documents.db").exists() {
            return Ok(Vec::new());
        }
        let space = q.space.clone();
        let text = q.text.clone();
        let limit = q.limit;

        // Channel 1+2: FTS word + ngram (space-scoped in the store).
        let word = {
            let dir = dir.clone();
            let space = space.clone();
            let text = text.clone();
            blocking(move || {
                let cache = DocumentCache::open_ro(&dir)?;
                cache.search_fts(&space, FtsTable::Word, &text, limit)
            })
            .await?
        };
        let ngram = {
            let dir = dir.clone();
            let space = space.clone();
            let text = text.clone();
            blocking(move || {
                let cache = DocumentCache::open_ro(&dir)?;
                cache.search_fts(&space, FtsTable::Ngram, &text, limit)
            })
            .await?
        };

        // Channel 3: dense — only when the mode wants it and an embedder
        let knn = match (
            matches!(q.mode, CoreQueryMode::Hybrid | CoreQueryMode::Dense),
            self.embedder.clone(),
        ) {
            (true, Some(embedder)) => {
                let dir = dir.clone();
                let text = text.clone();
                let out = blocking(move || {
                    let vectors = embedder
                        .embed(&[text.as_str()])
                        .map_err(|e| BrainError::Config(format!("embed: {e}")))?;
                    let v = vectors.into_iter().next().ok_or_else(|| {
                        BrainError::Config("embedding returned empty vector".into())
                    })?;
                    let cache = DocumentCache::open_ro(&dir)?;
                    cache.knn(&v, limit)
                })
                .await?;
                Some(out)
            }
            _ => None,
        };

        // RRF fusion (k=60) within the documents plane.
        let fused = rrf_fuse(&[&word, &ngram, knn.as_deref().unwrap_or(&[])], 60);
        if fused.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = fused.iter().map(|(id, _)| id.clone()).collect();
        let chunks: Vec<CachedChunk> = {
            let dir = dir.clone();
            blocking(move || {
                let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
                let cache = DocumentCache::open_ro(&dir)?;
                cache.chunks(&id_refs)
            })
            .await?
        };

        // Materialize per hit; knn is cross-space so enforce the space here.
        let fused_score: std::collections::HashMap<String, f64> = fused.into_iter().collect();
        let mut out: Vec<DocumentHit> = Vec::with_capacity(chunks.len());
        for ch in chunks {
            if ch.space != space {
                continue;
            }
            let score = fused_score.get(&ch.chunk_id).copied().unwrap_or(0.0);
            let text = match self.materialize_hit(&ch).await {
                Ok(Some(t)) => t,
                _ => continue, // stale or vanished — drop the hit
            };
            let reference = format!("doc://{}/{}?rev={}", ch.root_alias, ch.locator, ch.revision);
            out.push(DocumentHit {
                document_id: text.document_id,
                root: ch.root_alias,
                locator: ch.locator,
                revision: ch.revision,
                ordinal: ch.ordinal,
                text: UntrustedContent {
                    kind: "untrusted_content".to_string(),
                    text: text.text,
                    provenance: ContentProvenance {
                        reference,
                        trust: "unverified".to_string(),
                    },
                },
                modified_at: Timestamp::from_millis(ch.modified_at * 1000),
                score,
            });
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(out)
    }

    /// Resolve one cached chunk to a materialized slice. Returns `Ok(None)`
    /// when the file no longer matches the cached revision (stale, alias
    /// gone, unreadable, or span out of bounds) — the caller drops the hit
    /// (spec §12 invariant 12: never return mismatched text).
    async fn materialize_hit(
        &self,
        ch: &CachedChunk,
    ) -> Result<Option<HitMaterialized>, BrainError> {
        let canonical = match self.locator_to_root_path(&ch.root_alias) {
            Some(p) => p,
            None => return Ok(None),
        };
        let path = canonical.join(&ch.locator);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };

        // Revision verification: git revisions via
        // `GitDocumentReader::current_revision` against the bytes just read;
        // blake3 revisions via a direct hash compare. Any verification
        // failure drops the hit.
        let verified = if ch.revision.starts_with("git:") {
            match GitDocumentReader::open(&canonical) {
                Ok(Some(reader)) => match reader.snapshot() {
                    Ok(snap) => reader
                        .current_revision(&snap, &ch.locator, &bytes)
                        .map(|rev| rev == ch.revision)
                        .unwrap_or(false),
                    Err(_) => false,
                },
                _ => false,
            }
        } else {
            format!("blake3:{}", blake3_hex(&bytes)) == ch.revision
        };
        if !verified {
            return Ok(None);
        }

        // Choose the decoder by the STORED media type so cached chunk text
        // reproduces byte-for-byte: canonical PDC documents re-decode through
        // the PDC connector (envelope never enters the cached text); legacy
        // media types keep the legacy decoders. Unknown stored values fall
        // back to the locator extension.
        let media_type = MediaType::from_stored_str(&ch.media_type);
        let decoded_text: String = match ch.media_type.as_str() {
            s if s == BodyProfile::Djot.media_type() => match parse_djot_document("", &bytes) {
                Ok(doc) => doc.body.text,
                Err(_) => return Ok(None),
            },
            s if s == BodyProfile::Html.media_type() => match parse_html_document("", &bytes) {
                Ok(doc) => doc.body.text,
                Err(_) => return Ok(None),
            },
            _ => match decode(media_type.unwrap_or_else(|| media_type_of(&path)), &bytes) {
                Ok(d) => d.text,
                Err(_) => return Ok(None),
            },
        };
        // Span must stay inside the decoded text and align with UTF-8
        // boundaries; otherwise the cached row is not this file's shape.
        let span_start = ch.span_start.min(decoded_text.len());
        let span_end = ch.span_end.min(decoded_text.len());
        if span_start > span_end
            || !decoded_text.is_char_boundary(span_start)
            || !decoded_text.is_char_boundary(span_end)
        {
            return Ok(None);
        }
        Ok(Some(HitMaterialized {
            document_id: ch.document_id.clone(),
            text: decoded_text[span_start..span_end].to_string(),
        }))
    }

    // ─── root registration (unified-home boundary) ────────────────────────

    /// Idempotently register a document root on behalf of another app (the
    /// unified-home contract: oximemo and oxios never edit `documents.toml`
    /// themselves — they reach this operation through the client/stdio
    /// boundary). Upsert is keyed by `alias` with Added / Replaced /
    /// Unchanged semantics; omitted rules fall back to the connector
    /// defaults exactly like a hand-written entry with missing fields, and
    /// the save is atomic. Pure document-plane state: no `brain.db` access,
    /// no inference, no space row creation.
    pub async fn register_document_root(
        &self,
        spec: DocumentRootSpec,
    ) -> Result<RegistrationResult, BrainError> {
        let dir = self.config.dir.clone();
        blocking(move || {
            let entry = RootEntry {
                alias: spec.alias,
                path: spec.path,
                space: spec.space,
                include: spec.include.unwrap_or_else(default_include),
                exclude: spec.exclude.unwrap_or_else(default_exclude),
                max_file_bytes: spec.max_file_bytes.unwrap_or(DEFAULT_MAX_FILE_BYTES),
            };
            // Serialize the load-modify-save against the documents plane's
            // own writer lock: another serve child (or the CLI) may register
            // concurrently. Same bounded ladder as every write op.
            let mut last_err: Option<BrainError> = None;
            let _lock = 'acquire: {
                for delay_ms in [0u64, 25, 50, 100, 200, 400, 800] {
                    if delay_ms > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    }
                    match DocumentsLock::acquire(&dir) {
                        Ok(lock) => break 'acquire lock,
                        Err(e @ BrainError::Busy(_)) => last_err = Some(e),
                        Err(e) => return Err(e),
                    }
                }
                return Err(last_err.unwrap_or_else(|| BrainError::Busy("documents.lock".into())));
            };
            let mut cfg = load_documents_config(&dir)?;
            // Legacy configs may hold duplicate-alias blocks (flat-era
            // pollution). Repair toward the validate() invariant first,
            // keeping the operator's original entry per alias.
            let dropped = cfg.dedupe();
            let outcome = cfg.upsert(entry.clone());
            cfg.validate()
                .map_err(|e| BrainError::Config(e.to_string()))?;
            if outcome != UpsertOutcome::Unchanged || dropped > 0 {
                DocumentsConfig::save(&dir, &cfg).map_err(|e| BrainError::Config(e.to_string()))?;
            }
            let outcome = match outcome {
                UpsertOutcome::Added => RegisterRootOutcome::Added,
                UpsertOutcome::Replaced => RegisterRootOutcome::Replaced,
                UpsertOutcome::Unchanged => RegisterRootOutcome::Unchanged,
            };
            Ok(RegistrationResult {
                outcome,
                root: entry,
            })
        })
        .await
    }

    // ─── helpers ───────────────────────────────────────────────────────────

    /// Canonical on-disk path of a configured root alias (None when the
    /// alias is gone from `documents.toml` or its path no longer resolves).
    fn locator_to_root_path(&self, alias: &str) -> Option<PathBuf> {
        let cfg = load_documents_config(&self.config.dir).ok()?;
        let entry = cfg.root(alias)?;
        canonicalize_root(&entry.path).ok()
    }

    /// `(embedded, total)` chunk counts for one space; a missing cache is
    /// the zero state `(0, 0)`.
    async fn embedded_counts(&self, space: &str) -> Result<(u64, u64), BrainError> {
        let dir = self.config.dir.clone();
        let space = space.to_string();
        blocking(move || {
            Ok(match DocumentCache::open_ro(&dir) {
                Ok(cache) => cache.embedded_count(&space)?,
                Err(BrainError::NotFound(_)) => (0, 0),
                Err(e) => return Err(e),
            })
        })
        .await
    }

    /// `embedded / total` for one space; `None` when the space has no chunks.
    async fn dense_coverage(&self, space: &str) -> Result<Option<f64>, BrainError> {
        let (embedded, total) = self.embedded_counts(space).await?;
        Ok(ratio_or_none(embedded, total))
    }

    /// `embedded / total` aggregated over every configured space.
    async fn dense_coverage_all(&self) -> Result<Option<f64>, BrainError> {
        let dir = self.config.dir.clone();
        let spaces = configured_spaces(&dir)?;
        let mut embedded: u64 = 0;
        let mut total: u64 = 0;
        for space in spaces {
            let (e, t) = self.embedded_counts(&space).await?;
            embedded += e;
            total += t;
        }
        Ok(ratio_or_none(embedded, total))
    }
}

/// Coverage ratio; `None` when there are no chunks to cover.
fn ratio_or_none(embedded: u64, total: u64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(embedded as f64 / total as f64)
    }
}
// ─── Sequencer internals ────────────────────────────────────────────────────

struct RootIndexOutcome {
    alias: String,
    skipped_files: usize,
    root_apply: StoreRootApply,
    diagnostics: Vec<DiagnosticReport>,
    legacy_html: u32,
}

/// Outcome of materializing one Add/Replace action.
enum MaterializedUpsert {
    /// Canonical PDC document — the upsert carries the projection payload.
    Pdc(DocumentUpsert),
    /// Legacy decoder path (markdown, plain text, visible legacy HTML).
    Legacy(DocumentUpsert),
}

/// The decoded content of one Add/Replace action, tagged by decode path.
enum DecodedSource {
    /// Canonical PDC document — body text + projection payload come from
    /// the parsed document. Boxed: `PdcDocument` is far larger than the
    /// legacy variant.
    Pdc(Box<PdcDocument>),
    /// Legacy decoder output with its stored media type string.
    Legacy { media_type: String, text: String },
}

/// Why one Add/Replace action produced no upsert.
enum MaterializeError {
    /// The PDC connector rejected the document — a reportable diagnostic.
    Pdc(PdcDiagnostic),
    /// I/O or legacy-decode failure — counted as a skipped file.
    Io(BrainError),
}

struct HitMaterialized {
    document_id: String,
    text: String,
}

/// Assemble the manifest + chunk payload for one decoded Add/Replace action.
fn build_upsert(obs: &FileObservation, bytes: &[u8], source: DecodedSource) -> DocumentUpsert {
    let (text, media_type, pdc) = match source {
        DecodedSource::Pdc(doc) => {
            let meta = pdc_meta_from_document(&doc);
            let pdc = PdcProjectionUpsert {
                uuid: doc.metadata.document_uuid.clone(),
                body_profile: doc.metadata.body.as_str().to_string(),
                meta,
            };
            (
                doc.body.text,
                doc.metadata.body.media_type().to_string(),
                Some(pdc),
            )
        }
        DecodedSource::Legacy { media_type, text } => (text, media_type, None),
    };

    // Revision: gix-derived hint when present, else blake3 of the bytes
    // actually read (the same form materialize_hit verifies).
    let revision = obs
        .revision_hint
        .clone()
        .unwrap_or_else(|| format!("blake3:{}", blake3_hex(bytes)));

    let policy = ChunkPolicy::default();
    let chunks = split_into_chunks(&text, &policy);
    let mut store_chunks = Vec::with_capacity(chunks.len());
    for chunk in &chunks {
        // Chunk spans are UTF-8 byte offsets into `text`
        // (split_into_chunks guarantees boundary-aligned spans).
        let slice = &text[chunk.span_start..chunk.span_end];
        let context = render_context_prefix(
            Timestamp::from_millis((obs.modified_ns / 1_000_000).max(0)),
            "document",
            &[],
            None,
        );
        store_chunks.push(StoreChunkUpsert {
            ordinal: chunk.ordinal,
            span_start: chunk.span_start,
            span_end: chunk.span_end,
            context,
            text: slice.to_string(),
        });
    }

    DocumentUpsert {
        locator: obs.locator.clone(),
        revision,
        media_type,
        bytes: bytes.len() as u64,
        modified_ns: obs.modified_ns,
        // Seconds for display; hit materialization converts back to ms.
        modified_at: (obs.modified_ns / 1_000_000_000).max(0),
        chunks: store_chunks,
        pdc,
    }
}

/// Map a parsed PDC document to its projection metadata (the JSON shape
/// the store persists in `documents.pdc_meta`).
fn pdc_meta_from_document(doc: &PdcDocument) -> PdcProjectionMeta {
    PdcProjectionMeta {
        title: doc.metadata.title.clone(),
        display_title: doc.display_title.clone(),
        profile: doc.metadata.profile.clone(),
        lang: doc.metadata.lang.clone(),
        tags: doc.metadata.tags.clone(),
        aliases: doc.metadata.aliases.clone(),
        favorite: doc.metadata.favorite,
        deleted: doc.metadata.deleted,
        deleted_at: doc.metadata.deleted_at.clone(),
        created: doc.metadata.created.clone(),
        updated: doc.metadata.updated.clone(),
        links: doc
            .body
            .document_links
            .iter()
            .map(|l| PdcLinkMeta {
                uuid: l.uuid.clone(),
                block: l.block.clone(),
                embed: l.embed,
            })
            .collect(),
        assets: doc.body.asset_refs.clone(),
        task_count: doc.body.tasks.len() as u32,
        block_id_count: doc.body.block_ids.len() as u32,
        unsafe_flags: doc.body.unsafe_constructs.clone(),
    }
}

/// File stem of a locator — the connector's display-title fallback of last
/// resort.
fn locator_stem(locator: &str) -> String {
    Path::new(locator)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Diagnostic reason with the optional source position appended.
fn diagnostic_reason(d: &PdcDiagnostic) -> String {
    match (d.line, d.col) {
        (Some(line), Some(col)) => format!("{} (line {line}, col {col})", d.reason),
        (Some(line), None) => format!("{} (line {line})", d.reason),
        _ => d.reason.clone(),
    }
}

/// Maximum link targets listed in one `unresolved_link` reason before the
/// rest is summarized as "+N more".
const MAX_LISTED_LINK_TARGETS: usize = 5;

/// Build the informational `unresolved_link` report for one document:
/// the missing target UUIDs are aggregated into a single reason line.
fn unresolved_link_report(
    alias: &str,
    locator: &str,
    missing: &std::collections::BTreeSet<&str>,
) -> DiagnosticReport {
    let listed: Vec<&str> = missing
        .iter()
        .take(MAX_LISTED_LINK_TARGETS)
        .copied()
        .collect();
    let more = missing.len().saturating_sub(listed.len());
    DiagnosticReport {
        alias: alias.to_string(),
        locator: locator.to_string(),
        code: PdcDiagnosticCode::UnresolvedLink.as_str().to_string(),
        reason: format!(
            "{} unresolved pdc://document link target(s): {}{}",
            missing.len(),
            listed.join(", "),
            if more > 0 {
                format!(" (+{more} more)")
            } else {
                String::new()
            }
        ),
    }
}

/// Root-level PDC conflict pass (pure planning). A canonical UUID claimed
/// by more than one locator in the same pass has no canonical location —
/// the document owner's decision, never ours — so EVERY conflicting
/// locator is dropped: its upsert is removed, its action becomes `Skip`,
/// and it gets a `duplicate_document_id` diagnostic naming the other
/// claimants. Returns the surviving `uuid → locator` map.
fn resolve_duplicate_pdc_ids(
    alias: &str,
    actions: &mut [CoreFileAction],
    upserts: &mut Vec<DocumentUpsert>,
    action_of: &mut Vec<usize>,
    diagnostics: &mut Vec<DiagnosticReport>,
) -> std::collections::BTreeMap<String, String> {
    let mut by_uuid: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for (i, upsert) in upserts.iter().enumerate() {
        if let Some(pdc) = &upsert.pdc {
            by_uuid.entry(pdc.uuid.as_str()).or_default().push(i);
        }
    }

    let mut conflicting: std::collections::BTreeSet<usize> = Default::default();
    for (uuid, idxs) in &by_uuid {
        if idxs.len() < 2 {
            continue;
        }
        for &i in idxs {
            let others: Vec<&str> = idxs
                .iter()
                .filter(|&&j| j != i)
                .map(|&j| upserts[j].locator.as_str())
                .collect();
            diagnostics.push(DiagnosticReport {
                alias: alias.to_string(),
                locator: upserts[i].locator.clone(),
                code: PdcDiagnosticCode::DuplicateDocumentId.as_str().to_string(),
                reason: format!(
                    "document UUID {uuid} is also claimed by {}",
                    others.join(", ")
                ),
            });
            conflicting.insert(i);
        }
    }

    // Stable in-place partition: keep order, push dropped to the tail.
    // Converting the action here is what guarantees apply never sees an
    // Add/Replace without a matching upsert.
    let mut keep = 0usize;
    for i in 0..upserts.len() {
        if conflicting.contains(&i) {
            let locator = upserts[i].locator.clone();
            let uuid = upserts[i]
                .pdc
                .as_ref()
                .map(|p| p.uuid.clone())
                .unwrap_or_default();
            actions[action_of[i]] = CoreFileAction::Skip {
                locator,
                reason: format!("duplicate PDC document id {uuid} claimed by multiple locators"),
            };
            continue;
        }
        if keep != i {
            upserts.swap(keep, i);
            action_of.swap(keep, i);
        }
        keep += 1;
    }
    upserts.truncate(keep);
    action_of.truncate(keep);

    let mut uuid_map = std::collections::BTreeMap::new();
    for upsert in upserts.iter() {
        if let Some(pdc) = &upsert.pdc {
            uuid_map.insert(pdc.uuid.clone(), upsert.locator.clone());
        }
    }
    uuid_map
}

/// Verify one managed asset on disk: present under
/// `<root>/.pdc/assets/sha256/<2hex>/<digest>` and sha256-identical to the
/// referenced digest. The content hash is computed by streaming read —
/// assets are never loaded into memory whole. `Ok(())` = verified.
fn verify_managed_asset(root: &Path, digest: &str) -> Result<(), (PdcDiagnosticCode, String)> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err((
            PdcDiagnosticCode::AssetDigestMismatch,
            format!("malformed asset digest `{digest}`"),
        ));
    }
    let path = root
        .join(".pdc")
        .join("assets")
        .join("sha256")
        .join(&digest[..2])
        .join(digest);
    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => {
            return Err((
                PdcDiagnosticCode::MissingAsset,
                format!("asset {digest} not found at {}", path.display()),
            ));
        }
    };
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) => {
                return Err((
                    PdcDiagnosticCode::AssetDigestMismatch,
                    format!("asset {digest} read failed: {e}"),
                ));
            }
        }
    }
    let computed = hex::encode(hasher.finalize());
    if computed != digest {
        return Err((
            PdcDiagnosticCode::AssetDigestMismatch,
            format!("asset {digest} content hashes to {computed}"),
        ));
    }
    Ok(())
}

// ─── Free helpers ───────────────────────────────────────────────────────────

fn load_documents_config(dir: &Path) -> Result<DocumentsConfig, BrainError> {
    DocumentsConfig::load(dir).map_err(|e| BrainError::Config(e.to_string()))
}

/// Distinct spaces declared across all configured roots, sorted.
fn configured_spaces(dir: &Path) -> Result<Vec<String>, BrainError> {
    let cfg = load_documents_config(dir)?;
    let spaces: BTreeSet<String> = cfg.roots.iter().map(|r| r.space.clone()).collect();
    Ok(spaces.into_iter().collect())
}

/// Ship a blocking store operation to the tokio pool and flatten both
/// result layers (join error + operation error) into one `BrainError`.
async fn blocking<T, F>(f: F) -> Result<T, BrainError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, BrainError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
}

fn fingerprint_from_entry(entry: &RootEntry) -> Result<RootFingerprint, BrainError> {
    let canonical = canonicalize_root(&entry.path)?;
    Ok(RootFingerprint {
        alias: entry.alias.clone(),
        canonical_path: canonical.to_string_lossy().into_owned(),
        space: entry.space.clone(),
        include: entry.include.clone(),
        exclude: entry.exclude.clone(),
        max_file_bytes: entry.max_file_bytes,
        // Pins the decode semantics: a version bump invalidates every
        // cached fingerprint (equality change ⇒ one-time full rebuild).
        decoder_version: DECODER_VERSION.to_string(),
    })
}

/// Open the document cache read-write with a bounded backoff ladder on lock
/// contention (the advisory `documents.lock` may be held by another process
/// mid-apply). Only lock refusal is retried — store-level errors surface.
fn open_cache_rw_with_retry(dir: &Path) -> Result<DocumentCache, BrainError> {
    let mut last_err: Option<BrainError> = None;
    for delay_ms in [0u64, 25, 50, 100, 200, 400, 800] {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        match DocumentCache::open_rw(dir) {
            Ok(cache) => return Ok(cache),
            Err(e @ BrainError::Busy(_)) => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or(BrainError::Busy("documents.lock".into())))
}

fn stat_matches(path: &Path, obs: &FileObservation) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => {
            let ns = system_time_ns(&m.modified().unwrap_or(std::time::UNIX_EPOCH));
            m.len() == obs.bytes && ns == obs.modified_ns
        }
        Err(_) => false,
    }
}

fn system_time_ns(t: &std::time::SystemTime) -> i64 {
    let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    i64::try_from(dur.as_nanos()).unwrap_or(i64::MAX)
}

fn read_worktree_bytes(root: &Path, locator: &str) -> Result<Vec<u8>, BrainError> {
    std::fs::read(root.join(locator))
        .map_err(|e| BrainError::Storage(format!("read {locator}: {e}")))
}

fn blake3_hex(bytes: &[u8]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(bytes);
    hex::encode(h.finalize().as_bytes())
}

/// Media type from a path extension; unknown extensions decode as plain text.
fn media_type_of(path: &Path) -> MediaType {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(MediaType::from_extension)
        .unwrap_or(MediaType::PlainText)
}

/// Embedder identity for the documents-plane vector cache. `EmbeddingPort`
/// exposes only `dim()`/`embed()`, so the dimension is the identity — a
/// model swap that changes the dimension invalidates every cached vector;
/// same-dimension swaps keep vectors (accepted: stale-but-shaped beats
/// wiped indexes, and the manifest revision still gates hits).
fn embedder_identity(embedder: &dyn EmbeddingPort) -> String {
    format!("emb:dim:{}", embedder.dim())
}

/// Reciprocal Rank Fusion (k=60). Input rank lists are score-desc; lower
/// rank position ⇒ more weight. Returns the merged list sorted by fused
/// score desc; ties broken by stable ids.
fn rrf_fuse(lists: &[&[(String, f64)]], k: usize) -> Vec<(String, f64)> {
    let mut score: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for list in lists {
        for (rank, (id, _)) in list.iter().enumerate() {
            *score.entry(id.clone()).or_insert(0.0) += 1.0 / (k as f64 + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(String, f64)> = score.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Build a `doc://` URI for one hit. The locator path is percent-encoded
/// per the spec §6.7 (`doc://alias/<pct-locator>?rev=<revision>`).
fn build_doc_uri(alias: &str, locator: &str, revision: &str) -> String {
    let encoded_loc = encode_doc_locator(locator);
    format!("doc://{alias}/{encoded_loc}?rev={revision}")
}

/// Percent-encode a locator (spec §6.7). Encodes every byte outside the
/// RFC 3986 unreserved set (`A-Z a-z 0-9 - _ . ~`) and the `/` separator.
fn encode_doc_locator(locator: &str) -> String {
    let mut out = String::with_capacity(locator.len());
    for byte in locator.bytes() {
        let safe = matches!(
            byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/'
        );
        if safe {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used))]
    use super::*;

    fn pdc_upsert(locator: &str, uuid: &str) -> DocumentUpsert {
        DocumentUpsert {
            locator: locator.to_string(),
            revision: "rev1".to_string(),
            media_type: BodyProfile::Djot.media_type().to_string(),
            bytes: 10,
            modified_ns: 1,
            modified_at: 0,
            chunks: Vec::new(),
            pdc: Some(PdcProjectionUpsert {
                uuid: uuid.to_string(),
                body_profile: BodyProfile::Djot.as_str().to_string(),
                meta: PdcProjectionMeta::default(),
            }),
        }
    }

    fn add_action(locator: &str) -> CoreFileAction {
        CoreFileAction::Add(FileObservation {
            locator: locator.to_string(),
            bytes: 10,
            modified_ns: 1,
            revision_hint: None,
        })
    }

    #[test]
    fn duplicate_uuid_drops_every_conflicting_upsert() {
        let uuid_a = "018f47c6-4a77-7c52-9db8-0e5f9bcb17db";
        let uuid_c = "018f47c6-4a77-7c52-9db8-0e5f9bcb17dc";
        let mut actions = vec![
            add_action("a.djot"),
            add_action("b.djot"),
            add_action("c.djot"),
        ];
        let mut upserts = vec![
            pdc_upsert("a.djot", uuid_a),
            pdc_upsert("b.djot", uuid_a),
            pdc_upsert("c.djot", uuid_c),
        ];
        let mut action_of = vec![0usize, 1, 2];
        let mut diagnostics = Vec::new();

        let uuid_map = resolve_duplicate_pdc_ids(
            "vault",
            &mut actions,
            &mut upserts,
            &mut action_of,
            &mut diagnostics,
        );

        // The conflicted UUID vanishes from the map; the sole claimant stays.
        assert_eq!(uuid_map.len(), 1);
        assert_eq!(uuid_map.get(uuid_c).map(String::as_str), Some("c.djot"));
        // Every conflicting locator was dropped with a diagnostic naming
        // the other claimant.
        assert_eq!(upserts.len(), 1);
        assert_eq!(upserts[0].locator, "c.djot");
        assert_eq!(diagnostics.len(), 2);
        assert!(
            diagnostics
                .iter()
                .all(|d| d.code == PdcDiagnosticCode::DuplicateDocumentId.as_str())
        );
        let a = diagnostics.iter().find(|d| d.locator == "a.djot").unwrap();
        assert!(a.reason.contains("b.djot"), "reason: {}", a.reason);
        // Their actions became Skip so apply never sees an upsert-less
        // Add/Replace.
        assert!(matches!(&actions[0], CoreFileAction::Skip { locator, .. } if locator == "a.djot"));
        assert!(matches!(&actions[1], CoreFileAction::Skip { locator, .. } if locator == "b.djot"));
        assert!(matches!(&actions[2], CoreFileAction::Add(_)));
    }

    fn diag(alias: &str, code: &str) -> DiagnosticReport {
        DiagnosticReport {
            alias: alias.to_string(),
            locator: "x.djot".to_string(),
            code: code.to_string(),
            reason: "r".to_string(),
        }
    }

    #[test]
    fn diagnostic_lines_group_by_code_and_cap_with_note() {
        let diags = vec![
            diag("v1", "invalid_transport"),
            diag("v2", "invalid_transport"),
            diag("v3", "missing_asset"),
        ];
        let lines = format_diagnostic_lines(&diags, 50);
        assert_eq!(
            lines,
            vec![
                "invalid_transport:",
                "  v1/x.djot: r",
                "  v2/x.djot: r",
                "missing_asset:",
                "  v3/x.djot: r",
            ]
        );

        // Over the cap: one header + 3 diagnostics = 5 lines; cap 4 keeps
        // 3 and appends the omission note.
        let lines = format_diagnostic_lines(&diags, 4);
        assert_eq!(lines.len(), 4);
        assert!(lines.last().unwrap().contains("more diagnostic line"));
    }
}
