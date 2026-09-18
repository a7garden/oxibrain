//! oxibrain: the public facade. P6 — the engine is a library; every surface is an adapter.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod compat;
pub mod config;
pub mod document_plane;
mod extraction;
mod ingest;
pub mod migrate;
pub mod models;
pub mod paths;
pub mod pull_plan;
mod render;
pub use config::BrainConfig;
pub use document_plane::{
    DocumentFreshness, DocumentHit, DocumentRootSpec, IndexOptions, PendingStats,
    RegisterRootOutcome, RegistrationResult, SearchResponse,
};

pub use models::SpaceInfo;

pub use oxibrain_core::security::{
    AuditEntry, Capability, CapabilitySet, RedactTarget, RedactionClosure, RedactionResult, Scope,
    TokenInfo,
};
pub use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
pub use oxibrain_ports::{
    BrainError, CharTokenizer, ClockPort, EmbeddingPort, LlmPort, LlmRequest, LlmResponse,
    SystemClock, Timestamp, TokenizerPort,
};

use oxibrain_store::documents::DocumentCache;
pub use oxibrain_store::ledger::IngestAttachment;
pub use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
pub use oxibrain_store::security::AuditRow;
use oxibrain_store::{ledger, query};
use std::collections::HashMap;
use std::sync::Arc;

/// Discriminator for the three `brief` target kinds (M9 §14.1, `brief(entity |
/// space | topic)`). Entity targets go through `Brain::brief(space, entity_id)`
/// — the entity case is split out so the two-arg call stays a stable surface
/// for the existing UI clients. `Space` and `Topic` are reached via
/// `Brain::brief_target`.
#[derive(Debug, Clone, Copy)]
pub enum BriefTarget<'a> {
    Entity(&'a str),
    Space,
    Topic(&'a str),
}

/// The brain: a handle-free runtime facade (Daemonless Two-Plane §11).
///
/// A `Brain` value is configuration plus model ports — nothing else. It
/// holds **no** store connection, writer actor, or advisory lock. Every
/// method opens its store (`brain.db`, `documents.db`) for the duration of
/// one operation and drops it before returning, so any number of `Brain`
/// clones can coexist in one process and across processes.
///
/// Concurrency contract:
/// - Reads open a fresh read-only WAL connection per call — they never
///   block and never take a lock.
/// - Writes open the store under its advisory lock with a bounded retry
///   ladder (25, 50, 100, 200, 400, 800 ms; total wait ≈ 1.6 s) and then
///   fail with [`BrainError::Locked`]. Retrying the operation later is the
///   documented recovery path.
/// - No model/network/embedding call ever runs inside a transaction.
#[derive(Clone)]
pub struct Brain {
    config: BrainConfig,
    clock: Arc<dyn ClockPort>,
    llm: Option<Arc<dyn LlmPort>>,
    tokenizer: Arc<dyn TokenizerPort>,
    /// Optional dense embedder for QueryMode::Dense / hybrid dense channel.
    embedder: Option<Arc<dyn EmbeddingPort>>,
    /// Extractor identity (§9.5) used by inline capture and backlog drains.
    /// Defaults to the facade default; callers that resolve a concrete
    /// provider (model id, mechanism, weights digest) bind it here so a
    /// weight change invalidates the extraction cache instead of silently
    /// reusing extractions from the old weights.
    extractor_config: oxibrain_core::extraction::ExtractorConfig,
}

/// Outcome of a capture-family call (`remember`): either the episode was
/// captured *and* extracted inline, or it was captured and left on the
/// memory-plane backlog for `extract_uncached`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Episode appended; extraction succeeded inline. `extracted` counts
    /// the claims projected from this episode.
    Captured {
        episode_id: String,
        extracted: usize,
    },
    /// Episode appended; extraction did not run or failed (no LLM port,
    /// provider error, validation failure). The episode stays on the
    /// backlog; `pending` is the backlog size after this capture.
    CapturedPending { episode_id: String, pending: u64 },
}

impl Brain {
    /// Open (creating if needed) a brain directory. The open is a single
    /// short locked pass that applies migrations when `brain.db` is absent
    /// or outdated; no lock survives the call.
    pub async fn open(config: BrainConfig) -> Result<Self, BrainError> {
        Self::init_store_dir(config.dir.clone()).await?;
        Ok(Self::bare(config, Arc::new(SystemClock)))
    }

    pub async fn with_clock(
        config: BrainConfig,
        clock: Arc<dyn ClockPort>,
    ) -> Result<Self, BrainError> {
        Self::init_store_dir(config.dir.clone()).await?;
        Ok(Self::bare(config, clock))
    }

    fn bare(config: BrainConfig, clock: Arc<dyn ClockPort>) -> Self {
        Self {
            config,
            clock,
            llm: None,
            tokenizer: Arc::new(CharTokenizer),
            embedder: None,
            extractor_config: crate::extraction::default_extractor_config(),
        }
    }

    /// One short locked open that creates the directory + `brain.db` and
    /// applies any outstanding migrations. Called by the constructors only.
    async fn init_store_dir(dir: std::path::PathBuf) -> Result<(), BrainError> {
        tokio::task::spawn_blocking(move || -> Result<(), BrainError> {
            let _store = oxibrain_store::Store::open(&dir)?;
            Ok(())
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Open for read-only access (DESIGN §4.3). No advisory lock is ever
    /// taken; all write methods fail fast with a Config error. Reads can
    /// coexist with a writer in another process (WAL).
    pub async fn open_ro(config: BrainConfig) -> Result<Self, BrainError> {
        let dir = config.dir.clone();
        let probe = tokio::task::spawn_blocking(move || {
            oxibrain_store::Store::open_read_only(&dir).map(|_| ())
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?;
        probe?;
        let mut cfg = config;
        cfg.read_only = true;
        Ok(Self::bare(cfg, Arc::new(SystemClock)))
    }

    /// Create a Brain with a custom clock and LLM port.
    pub async fn with_llm(
        config: BrainConfig,
        clock: Arc<dyn ClockPort>,
        llm: Arc<dyn LlmPort>,
    ) -> Result<Self, BrainError> {
        Self::init_store_dir(config.dir.clone()).await?;
        let mut brain = Self::bare(config, clock);
        brain.llm = Some(llm);
        Ok(brain)
    }

    /// Create a Brain with a custom clock, LLM port, and tokenizer. The
    /// local model path (§7.5) passes the model's own tokenizer so token
    /// budgets are counted, not estimated.
    pub async fn with_llm_and_tokenizer(
        config: BrainConfig,
        clock: Arc<dyn ClockPort>,
        llm: Arc<dyn LlmPort>,
        tokenizer: Arc<dyn TokenizerPort>,
    ) -> Result<Self, BrainError> {
        Self::init_store_dir(config.dir.clone()).await?;
        let mut brain = Self::bare(config, clock);
        brain.llm = Some(llm);
        brain.tokenizer = tokenizer;
        Ok(brain)
    }

    /// Attach a dense embedder for QueryMode::Dense / hybrid dense channel
    /// (§7.6) and the documents-plane vector channel.
    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingPort>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Bind the extractor identity used by inline capture (`remember`) and
    /// backlog drains (`extract_uncached`). Fold the resolved provider's
    /// model id, mechanism, and weights digest in (§9.5): the ExtractorId
    /// keys the extraction cache, so a model or weight change must change
    /// it or stale extractions from the old weights keep hitting.
    pub fn with_extractor_config(
        mut self,
        config: oxibrain_core::extraction::ExtractorConfig,
    ) -> Self {
        self.extractor_config = config;
        self
    }

    /// The bound extractor identity.
    pub(crate) fn extractor_config(&self) -> &oxibrain_core::extraction::ExtractorConfig {
        &self.extractor_config
    }

    /// Returns the configured embedder, or None.
    pub fn embedder(&self) -> Option<&Arc<dyn EmbeddingPort>> {
        self.embedder.as_ref()
    }

    /// Tokenizer identity (e.g. `"qwen2.5"` or `"char-fallback"`) — the
    /// CLI/MCP envelope reports this in `meta.tokens.counted_by` so the
    /// caller knows whether the budget is exact or estimated (§7.5).
    pub fn tokenizer_id(&self) -> &str {
        self.tokenizer.id()
    }

    /// Returns the configured LLM port, or an error if none.
    fn require_llm(&self) -> Result<&Arc<dyn LlmPort>, BrainError> {
        self.llm
            .as_ref()
            .ok_or_else(|| BrainError::Config("no LLM port configured".into()))
    }

    /// Brain directory (the store root every method re-opens).
    pub fn dir(&self) -> &std::path::Path {
        &self.config.dir
    }

    // ── Operation-scoped store access (P8 op-scoped) ─────────────────────

    /// Run a read closure on a fresh read-only connection to `brain.db`.
    /// The connection is opened per call and dropped at the end — no
    /// pooled state survives the operation.
    pub(crate) async fn read<T, F>(&self, f: F) -> Result<T, BrainError>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T, BrainError> + Send + 'static,
    {
        let dir = self.config.dir.clone();
        tokio::task::spawn_blocking(move || {
            let conn = oxibrain_store::Store::open_read_only(&dir)?;
            f(&conn)
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Run a write closure inside one transaction on a fresh locked
    /// connection to `brain.db`. Lock acquisition retries with backoff
    /// [25, 50, 100, 200, 400, 800] ms; exhaustion surfaces
    /// [`BrainError::Locked`]. The lock is held only for the transaction —
    /// the store (and its advisory lock) drops before this returns.
    pub(crate) async fn write<T, F>(&self, f: F) -> Result<T, BrainError>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T, BrainError> + Send + 'static,
    {
        if self.config.read_only {
            return Err(BrainError::Config("store is read-only".into()));
        }
        let dir = self.config.dir.clone();
        tokio::task::spawn_blocking(move || -> Result<T, BrainError> {
            let mut last_err: Option<BrainError> = None;
            for delay_ms in [0u64, 25, 50, 100, 200, 400, 800] {
                if delay_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
                // The store owns the advisory lock; keeping it alive for
                // the transaction's duration is what makes this write
                // exclusive. `unchecked_transaction` borrows the inner
                // connection without claiming mutability.
                match oxibrain_store::Store::open(&dir) {
                    Ok(store) => {
                        let tx = store
                            .connection()
                            .unchecked_transaction()
                            .map_err(|e| BrainError::Storage(format!("begin: {e}")))?;
                        let out = f(&tx)?;
                        tx.commit()
                            .map_err(|e| BrainError::Storage(format!("commit: {e}")))?;
                        return Ok(out);
                    }
                    Err(e @ BrainError::Locked { .. }) => {
                        last_err = Some(e);
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(last_err.unwrap_or(BrainError::Locked {
                holder: "unknown".into(),
            }))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    // ── Spaces ────────────────────────────────────────────────────────────

    /// Ensure a space exists. Returns its id.
    pub async fn ensure_space(&self, name: &str) -> Result<String, BrainError> {
        let now = self.clock.now();
        let name = name.to_string();
        self.write(move |conn| ledger::create_space(conn, &name, now))
            .await
    }

    /// Look up a space id by name without creating it (read-only). `Ok(None)`
    /// when the name has no row. Scope gates call this instead of
    /// `ensure_space` so denied reads do not create shadow rows.
    pub async fn lookup_space(&self, name: &str) -> Result<Option<String>, BrainError> {
        let name = name.to_string();
        self.read(move |conn| ledger::lookup_space_by_name(conn, &name))
            .await
    }

    /// Drop a space row (verified empty by the caller). Spec §4.5.
    pub async fn drop_space(&self, space_id: &str) -> Result<(), BrainError> {
        let space_id = space_id.to_string();
        self.write(move |conn| ledger::drop_space(conn, &space_id))
            .await
    }

    /// List all spaces with live counts, ordered by (created_at, id).
    pub async fn list_spaces(&self) -> Result<Vec<SpaceInfo>, BrainError> {
        self.read(move |conn| {
            let rows = ledger::list_spaces(conn)?;
            Ok(rows
                .into_iter()
                .map(|r| SpaceInfo {
                    id: r.id,
                    name: r.name,
                    created_at: Timestamp::from_millis(r.created_at),
                    episode_count: r.episode_count,
                    entity_count: r.entity_count,
                })
                .collect())
        })
        .await
    }

    /// Predicate registry as persisted in the store — the core/v1 seed plus
    /// every custom registration — ordered by name. The read path for
    /// registry introspection (`admin predicate list`); semantics stay in
    /// the registry (P4).
    pub async fn list_predicates(&self) -> Result<Vec<oxibrain_core::PredicateDef>, BrainError> {
        self.read(move |conn| {
            let mut defs: Vec<oxibrain_core::PredicateDef> =
                oxibrain_store::registry::load_all_predicates(conn)?
                    .into_values()
                    .collect();
            defs.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(defs)
        })
        .await
    }

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

    /// Ingest a note episode. Returns the episode id (content-derived).
    pub async fn ingest_note(
        &self,
        space: &str,
        path: &str,
        content: String,
        occurred_at: Timestamp,
    ) -> Result<String, BrainError> {
        self.ingest_note_impl(space, path, content, occurred_at)
            .await
    }

    /// Note content hashes per source path (live episodes only), for sync
    /// classification. Read-only; the decision lives in
    /// [`oxibrain_core::classify_sync`].
    pub async fn note_hashes(&self, space: &str) -> Result<oxibrain_core::KnownNotes, BrainError> {
        let space = space.to_string();
        self.read(move |conn| ledger::note_hashes_by_path(conn, &space))
            .await
    }

    /// Latest event-path episode state per locator for a source.
    /// Used by sync to derive occurrence chains (§4.2 pull mode).
    pub async fn locator_states(
        &self,
        space: &str,
        source_id: &str,
    ) -> Result<HashMap<String, oxibrain_core::sync::LocatorState>, BrainError> {
        let space = space.to_string();
        let source_id = source_id.to_string();
        self.read(move |conn| ledger::locator_states(conn, &space, &source_id))
            .await
    }

    /// Current time from the configured clock. Exposed for callers that need
    /// a Timestamp without going through an ingest method.
    pub fn clock_now(&self) -> Timestamp {
        self.clock.now()
    }

    pub async fn get_episode(&self, id: &str) -> Result<Option<Episode>, BrainError> {
        let id = id.to_string();
        self.read(move |conn| ledger::get_episode(conn, &id)).await
    }

    pub async fn episode_count(&self) -> Result<i64, BrainError> {
        self.read(ledger::episode_count).await
    }

    // ── Projection lifecycle ──────────────────────────────────────────────

    /// Drop and rebuild the entire projection from the ledger. When an
    /// embedder is configured, dense entity vectors are recomputed after
    /// the reproject pass (§7.6, F17): entity texts read via a read-only
    /// connection, embeddings computed outside any store lock, upserts
    /// written in a second short write op.
    pub async fn reproject(&self) -> Result<(), BrainError> {
        self.write(oxibrain_store::reproject).await?;

        // Embed after reproject, outside the writer transaction.
        if let Some(emb) = self.embedder.clone() {
            // Phase 1: read entity texts (read-only connection).
            let items: Vec<(String, String)> = self
                .read(move |conn| {
                    let mut stmt = conn
                        .prepare("SELECT id FROM spaces ORDER BY id")
                        .map_err(|e| BrainError::Storage(format!("space list: {e}")))?;
                    let spaces: Vec<String> = stmt
                        .query_map([], |r| r.get(0))
                        .map_err(|e| BrainError::Storage(format!("space list: {e}")))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| BrainError::Storage(format!("space list: {e}")))?;
                    drop(stmt);
                    let mut all: Vec<(String, String)> = Vec::new();
                    for space in spaces {
                        let per_space =
                            oxibrain_store::index_ops::entity_embedding_texts(conn, &space)?;
                        all.extend(per_space);
                    }
                    Ok(all)
                })
                .await?;
            if !items.is_empty() {
                // Phase 2: compute embeddings outside any writer lock.
                let text_refs: Vec<&str> = items.iter().map(|(_, t)| t.as_str()).collect();
                let vectors = emb
                    .embed(&text_refs)
                    .map_err(|e| BrainError::Config(format!("entity embedding: {e}")))?;
                let with_vectors: Vec<(String, Vec<f32>)> = items
                    .into_iter()
                    .zip(vectors)
                    .map(|((id, _), v)| (id, v))
                    .collect();
                // Phase 3: upsert via one short write op.
                self.write(move |conn| {
                    oxibrain_store::index_ops::upsert_entity_embeddings(conn, &with_vectors)
                })
                .await?;
            }
        }
        Ok(())
    }

    /// Declare a statement, merge, or retraction. Returns the episode id.
    ///
    /// The resolution cache is per-call: the handle-free facade holds no
    /// process-lifetime state, so each declaration builds its LSH entries
    /// on first touch (`get_or_build` reads the keys for one (space, type)
    /// pair only — the amortization the persistent cache provided lives on
    /// inside a single call's key set).
    pub async fn declare(&self, space: &str, decl: Declaration) -> Result<String, BrainError> {
        let space = space.to_string();
        let now = self.clock.now();
        self.write(move |conn| {
            let mut cache = oxibrain_store::project::ResolutionCache::new();
            oxibrain_store::project::project_declaration(conn, &space, &decl, now, &mut cache)
        })
        .await
    }

    // ── Reads (truth + ranking halves) ────────────────────────────────────

    /// Current beliefs for an entity (follows merge chain).
    pub async fn beliefs(
        &self,
        space: &str,
        entity_id: &str,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| query::beliefs_for_entity(conn, &space, &entity_id))
            .await
    }

    /// Canonical display surface for an entity id (follows merge chain).
    /// Used by MCP projections that render human-readable object names.
    pub async fn entity_surface(&self, space: &str, entity_id: &str) -> Result<String, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| {
            oxibrain_store::brief::surface_of(conn, &entity_id)
                .map_err(|_| BrainError::NotFound(format!("entity {entity_id} in {space}")))
        })
        .await
    }

    /// Rebuild a Retract declaration's inputs from a stored statement —
    /// the statement-first retract path (the conflicts inbox holds statement
    /// ids, not resolvable surfaces). Pure read; the caller declares.
    pub async fn retract_parts(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<
        (
            oxibrain_store::project::EntityRef,
            String,
            oxibrain_store::project::DeclObject,
        ),
        BrainError,
    > {
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        self.read(move |conn| query::retract_parts(conn, &space, &statement_id))
            .await
    }

    /// Beliefs as of a valid-time point.
    pub async fn beliefs_as_of(
        &self,
        space: &str,
        entity_id: &str,
        valid_at: oxibrain_ports::Timestamp,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| query::beliefs_as_of(conn, &space, &entity_id, Some(valid_at), None))
            .await
    }

    /// All contradicted statements in a space.
    pub async fn contradictions(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::Statement>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| query::contradictions(conn, &space))
            .await
    }

    /// Contradicted statements with surfaces and supporting episodes (UI DTO).
    pub async fn contradiction_details(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::query::ContradictionDetail>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::query::contradiction_details(conn, &space))
            .await
    }

    /// Aggregate counts for a space (episodes, entities, statements,
    /// contradicted statements). Used by the `stats` MCP tool and dashboards.
    pub async fn stats(&self, space: &str) -> Result<oxibrain_core::SpaceStats, BrainError> {
        let space = space.to_string();
        self.read(move |conn| query::space_stats(conn, &space))
            .await
    }

    /// Hybrid (or mode-specific) query. Returns ranked results with provenance.
    pub async fn query(
        &self,
        q: oxibrain_core::retrieval::Query,
    ) -> Result<oxibrain_core::retrieval::RankingResult, BrainError> {
        let embedder = self.embedder.clone();
        self.read(move |conn| query::hybrid_query(conn, &q, embedder.as_deref()))
            .await
    }

    /// Memory-plane search for the planes-aware `search` (document_plane).
    /// Hybrid query projected to UI-ready entity hits. Legacy
    /// `document` / `document_revision` episodes are excluded at the FTS
    /// fetch (see `query::fts_search`).
    pub(crate) async fn search_memory(
        &self,
        q: &oxibrain_core::retrieval::Query,
    ) -> Result<Vec<oxibrain_store::query::SearchResult>, BrainError> {
        let embedder = self.embedder.clone();
        let q = q.clone();
        self.read(move |conn| {
            let ranking = query::hybrid_query(conn, &q, embedder.as_deref())?;
            query::search_results(conn, &q.space, &ranking)
        })
        .await
    }

    pub async fn rebuild_indexes(&self, space: &str) -> Result<(), BrainError> {
        let space = space.to_string();
        self.write(move |conn| oxibrain_store::index_ops::rebuild_indexes(conn, &space))
            .await
    }

    /// Bounded subgraph traversal over a space's statement graph.
    pub async fn traverse(
        &self,
        space: &str,
        spec: oxibrain_core::retrieval::TraversalSpec,
    ) -> Result<oxibrain_core::retrieval::TraversalResult, BrainError> {
        let space = space.to_string();
        self.read(move |conn| query::traverse(conn, &space, &spec))
            .await
    }

    /// Look up the entity_id for a surface form + type within a space. Returns
    /// `None` if the entity hasn't been declared yet.
    pub async fn resolve_entity_id(
        &self,
        space: &str,
        ty: &str,
        surface: &str,
    ) -> Result<Option<String>, BrainError> {
        let space = space.to_string();
        let ty = ty.to_string();
        let surface = surface.to_string();
        self.read(move |conn| query::resolve_entity_id(conn, &space, &ty, &surface))
            .await
    }

    pub async fn list_entities(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_core::Entity>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::knowledge::list_entities(conn, &space, limit))
            .await
    }

    /// Entities with canonical surfaces resolved, for the `space://` resource.
    /// One SQL join — no N+1.
    pub async fn list_entity_cards(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_store::knowledge::EntityCard>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::knowledge::list_entity_cards(conn, &space, limit))
            .await
    }

    /// List merge records in a space, most recent first.
    /// Used by the `review_merges` MCP tool.
    pub async fn list_merges(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::EntityMerge>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::knowledge::list_merges(conn, &space))
            .await
    }

    /// List extraction failures in a space, most recent first.
    /// Used by the `review_merges` MCP tool (`section: "failures"`).
    pub async fn list_failures(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::quarantine::ExtractionFailure>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::quarantine::list_failures(conn, Some(&space)))
            .await
    }

    /// List registered sources in a space.
    /// Used by the `review_merges` MCP tool (`section: "sources"`).
    pub async fn list_sources(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::ledger::SourceRow>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::ledger::list_sources(conn, &space))
            .await
    }

    /// Source-registry rows that never produced an episode (doctor report).
    pub async fn orphan_source_count(&self) -> Result<i64, BrainError> {
        self.read(oxibrain_store::ledger::count_orphan_sources)
            .await
    }

    pub async fn timeline(
        &self,
        space: &str,
        entity_id: &str,
        from: Option<oxibrain_ports::Timestamp>,
        to: Option<oxibrain_ports::Timestamp>,
    ) -> Result<Vec<oxibrain_store::timeline::TimelineEntry>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| {
            oxibrain_store::timeline::timeline(conn, &space, &entity_id, from, to)
        })
        .await
    }

    pub async fn diff(
        &self,
        space: &str,
        entity_id: &str,
        at_a: oxibrain_ports::Timestamp,
        at_b: oxibrain_ports::Timestamp,
    ) -> Result<oxibrain_store::timeline::DiffResult, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| oxibrain_store::timeline::diff(conn, &space, &entity_id, at_a, at_b))
            .await
    }

    pub async fn why(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<oxibrain_store::explain::ExplainBlock, BrainError> {
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        self.read(move |conn| oxibrain_store::explain::why(conn, &space, &statement_id))
            .await
    }

    // ── Briefs ────────────────────────────────────────────────────────────

    /// Render an entity page (`brief`) as Markdown with followable links
    /// (§14.1, M9 §9.2). Pure fetch + pure render — deterministic.
    pub async fn brief(&self, space: &str, entity_id: &str) -> Result<String, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        let data = self
            .read(move |conn| oxibrain_store::brief::entity_brief(conn, &space, &entity_id))
            .await?;
        Ok(render::render_entity_brief(&data))
    }

    /// Follow a followable link from a page (§14.1, M9 §9.3). `link` is either
    /// a raw entity id or an `entity://<id>` link; returns that entity's brief.
    pub async fn navigate(
        &self,
        space: &str,
        _from: &str,
        link: &str,
    ) -> Result<String, BrainError> {
        let target = oxibrain_views::parse_entity_link(link).unwrap_or(link);
        if target.is_empty() {
            return Err(BrainError::Config(format!("invalid link: {link}")));
        }
        self.brief(space, target).await
    }

    /// Render a brief for a non-entity target (M9 §14.1, `brief(space)`,
    /// `brief(topic)`). For entity targets, prefer `brief(space, entity_id)`
    /// — this method does not cover the entity case to keep the dispatch
    /// explicit at the call site.
    pub async fn brief_target(
        &self,
        space: &str,
        target: BriefTarget<'_>,
    ) -> Result<String, BrainError> {
        use oxibrain_store::brief as b;
        // Materialize the borrowed `target` as owned strings before the
        // `move` closure — the closure must satisfy `'static`.
        let kind: String = match target {
            BriefTarget::Entity(_) => {
                return Err(BrainError::Config(
                    "use Brain::brief(space, entity_id) for entity targets".into(),
                ));
            }
            BriefTarget::Space => "space".to_string(),
            BriefTarget::Topic(t) => format!("topic:{t}"),
        };
        let space = space.to_string();
        self.read(move |conn| -> Result<String, BrainError> {
            if let Some(topic) = kind.strip_prefix("topic:") {
                let data = b::topic_brief(conn, &space, topic, 50)?;
                Ok(render::render_topic_brief(&data))
            } else {
                // kind == "space"
                let data = b::space_brief(conn, &space, 50)?;
                Ok(render::render_space_brief(&data))
            }
        })
        .await
    }

    /// Byte-identical snapshot of the truth half (P1, §5.1).
    pub async fn snapshot_truth(&self, space: &str) -> Result<String, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::index_ops::snapshot_truth(conn, &space))
            .await
    }

    /// Equivalent snapshot of the ranking half (P1, §5.1).
    pub async fn snapshot_ranking(&self, space: &str) -> Result<String, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::index_ops::snapshot_ranking(conn, &space))
            .await
    }

    pub async fn rebuild_communities(&self, space: &str) -> Result<(), BrainError> {
        let space = space.to_string();
        self.write(move |conn| oxibrain_store::communities::rebuild_communities(conn, &space))
            .await
    }

    pub async fn community_members(
        &self,
        space: &str,
        entity_id: &str,
    ) -> Result<Vec<String>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| {
            oxibrain_store::communities::community_members(conn, &space, &entity_id)
        })
        .await
    }

    pub async fn apply_decay(&self, space: &str) -> Result<usize, BrainError> {
        let space = space.to_string();
        let now = self.clock.now();
        let config = oxibrain_core::lifecycle::DecayConfig::default();
        self.write(move |conn| oxibrain_store::lifecycle::apply_decay(conn, &space, now, &config))
            .await
    }

    pub async fn compact(&self, space: &str) -> Result<usize, BrainError> {
        let space = space.to_string();
        let now = self.clock.now();
        self.write(move |conn| oxibrain_store::lifecycle::compact_episodes(conn, &space, now, 90))
            .await
    }

    pub async fn assemble_context(
        &self,
        space: &str,
        query: &str,
        token_budget: usize,
    ) -> Result<oxibrain_core::context::ContextResult, BrainError> {
        self.assemble_context_with(space, query, token_budget, None)
            .await
    }

    /// Proactive recall: assemble context with hint-driven layer composition
    /// (DESIGN §9.5, sub-project L3). When `is_session_start` or `topic_changed`
    /// is true, the context widens to include more recent episodes and
    /// community summaries.
    pub async fn assemble_context_with_hints(
        &self,
        space: &str,
        query: &str,
        token_budget: usize,
        hints: &oxibrain_store::context::RecallHints,
    ) -> Result<oxibrain_core::context::ContextResult, BrainError> {
        self.assemble_context_with(space, query, token_budget, Some(hints))
            .await
    }

    async fn assemble_context_with(
        &self,
        space: &str,
        query: &str,
        token_budget: usize,
        hints: Option<&oxibrain_store::context::RecallHints>,
    ) -> Result<oxibrain_core::context::ContextResult, BrainError> {
        let tokenizer = self.tokenizer.clone();
        let space = space.to_string();
        let query = query.to_string();
        let hints = hints.cloned();
        self.read(move |conn| {
            oxibrain_store::context::assemble_context(
                conn,
                &space,
                &query,
                token_budget,
                hints.as_ref(),
                tokenizer.as_ref(),
            )
        })
        .await
    }

    // ── Ingest / extraction delegation ────────────────────────────────────

    /// Ingest an episode and index it for lexical search. Returns the episode
    /// id. Queue-less since v11: extraction runs inline (see `remember`) or
    /// via the uncached backlog (`extract_uncached`).
    pub async fn ingest(
        &self,
        space: &str,
        content: String,
        source: SourceRef,
        trust: TrustTier,
        extractor_id: &str,
    ) -> Result<String, BrainError> {
        self.ingest_impl(space, content, source, trust, extractor_id)
            .await
    }

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
        self.ingest_event_impl(
            space,
            content,
            source,
            trust,
            attachment.cloned(),
            extractor_id,
        )
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

    // ── Security ──────────────────────────────────────────────────────────

    /// Issue a token. Returns (TokenInfo, secret). The secret is shown once.
    pub async fn issue_token(
        &self,
        scope: &Scope,
        issued_by: &str,
        label: Option<&str>,
    ) -> Result<(TokenInfo, String), BrainError> {
        let now = self.clock.now();
        let scope = scope.clone();
        let issued_by = issued_by.to_string();
        let label = label.map(String::from);
        self.write(move |conn| {
            oxibrain_store::security::issue_token(conn, &scope, &issued_by, label.as_deref(), now)
        })
        .await
    }

    /// Verify a token by its secret. Returns the scope if valid and not
    /// expired/revoked.
    pub async fn verify_token(&self, secret: &str) -> Result<Option<Scope>, BrainError> {
        let now = self.clock.now();
        let secret = secret.to_string();
        self.read(move |conn| oxibrain_store::security::verify_token(conn, &secret, now))
            .await
    }

    /// Revoke a token by id.
    pub async fn revoke_token(&self, id: &str) -> Result<(), BrainError> {
        let now = self.clock.now();
        let id = id.to_string();
        self.write(move |conn| oxibrain_store::security::revoke_token(conn, &id, now))
            .await
    }

    /// List all tokens (active and revoked).
    pub async fn list_tokens(&self) -> Result<Vec<TokenInfo>, BrainError> {
        self.read(oxibrain_store::security::list_tokens).await
    }

    /// List recent audit entries, most recent first.
    pub async fn audit_log(&self, limit: Option<i64>) -> Result<Vec<AuditRow>, BrainError> {
        self.read(move |conn| oxibrain_store::security::list_audit(conn, limit))
            .await
    }

    // ── Redaction ─────────────────────────────────────────────────────────

    /// Resolve the closure of objects affected by redacting `target`. Does NOT
    /// modify the store — safe for `--dry-run`.
    pub async fn redact_dry_run(
        &self,
        target: &RedactTarget,
    ) -> Result<RedactionClosure, BrainError> {
        let target = target.clone();
        self.read(move |conn| oxibrain_store::redaction::resolve_closure(conn, &target))
            .await
    }

    /// Execute redaction. Writes audit + redactions record FIRST, then
    /// tombstones and deletes. Returns what was affected.
    pub async fn redact(
        &self,
        target: &RedactTarget,
        reason: &str,
        actor: &str,
    ) -> Result<RedactionResult, BrainError> {
        let now = self.clock.now();
        let target = target.clone();
        let reason = reason.to_string();
        let actor = actor.to_string();
        self.write(move |conn| {
            oxibrain_store::redaction::execute_redaction(conn, &target, &reason, &actor, now)
        })
        .await
    }

    /// True if a `redactions` tombstone exists for `target` — the audited
    /// proof that a redaction already ran. `space remove --purge` re-runs
    /// use the `Space` tombstone to tell a crash-resumed purge (brain-side
    /// redaction done, cache sweep pending) from a space that never existed
    /// (spec §4.5 crash recovery).
    pub async fn redaction_recorded(&self, target: &RedactTarget) -> Result<bool, BrainError> {
        let target_json = serde_json::to_string(target)
            .map_err(|e| BrainError::Storage(format!("serialize redact target: {e}")))?;
        self.read(move |conn| oxibrain_store::security::redaction_recorded(conn, &target_json))
            .await
    }

    // ── Export / import ───────────────────────────────────────────────────

    /// Export all durable tables as a JSONL string.
    pub async fn export_jsonl(&self) -> Result<String, BrainError> {
        self.read(oxibrain_store::export::export_jsonl).await
    }

    /// Import JSONL into the store. Assumes the store is fresh (tables empty).
    /// Pre-v11 `ingest_jobs` lines are skipped and counted in the summary.
    pub async fn import_jsonl(
        &self,
        jsonl: String,
    ) -> Result<oxibrain_store::export::ImportSummary, BrainError> {
        self.write(move |conn| oxibrain_store::export::import_jsonl(conn, &jsonl))
            .await
    }

    // ── Extraction delegation ─────────────────────────────────────────────

    /// Extract a single episode synchronously (realtime mode). Uses the
    /// configured LLM provider. See [`extract_one_with`](Self::extract_one_with)
    /// for the variant that takes an explicit provider (e.g. MCP sampling).
    pub async fn extract_one(
        &self,
        space: &str,
        episode_id: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let llm = self.require_llm()?.clone();
        self.extract_one_with(space, episode_id, config, llm).await
    }

    /// Extract a single episode synchronously with an explicit LLM provider.
    ///
    /// Does NOT use the job queue — directly reads, calls the provided LLM,
    /// validates, projects. Used by the realtime MCP sampling path (§12.3):
    /// the `llm` is a [`SamplingLlmPort`](../../oxibrain_mcp/sampling/struct.SamplingLlmPort.html)
    pub async fn extract_one_with(
        &self,
        space: &str,
        episode_id: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
        llm: std::sync::Arc<dyn LlmPort>,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        self.extract_one_with_impl(space, episode_id, config, llm)
            .await
    }

    /// Re-extract all primary episodes with a new extractor config.
    /// Old cache entries are preserved (different extractor_id = different PK).
    pub async fn reextract(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        self.reextract_impl(space, config).await
    }

    /// Clusters episodes by shared entities → LLM summarize → Derived episode.
    pub async fn consolidate(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<Vec<String>, BrainError> {
        self.consolidate_impl(space, config).await
    }

    /// Generate community summary text as cached Derived episodes (§9.4, §5.3).
    pub async fn summarize_communities(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<usize, BrainError> {
        self.summarize_communities_impl(space, config).await
    }

    // ── Debug / eval ──────────────────────────────────────────────────────

    /// Render statements by id as `id | subject predicate object` text.
    /// Used by the gate runner to score ranked items against answers.
    pub async fn render_statements(
        &self,
        space: &str,
        ids: &[String],
    ) -> Result<Vec<String>, BrainError> {
        let space = space.to_string();
        let ids = ids.to_vec();
        self.read(move |conn| oxibrain_store::query::render_statements(conn, &space, &ids))
            .await
    }

    /// Look up statement IDs where the given entities are subject or object.
    pub async fn statements_for_entities(
        &self,
        space: &str,
        entity_ids: &[String],
    ) -> Result<Vec<String>, BrainError> {
        let space = space.to_string();
        let eids = entity_ids.to_vec();
        self.read(move |conn| oxibrain_store::query::statements_for_entities(conn, &space, &eids))
            .await
    }

    /// Extract all triples from a space's current projection.
    pub async fn debug_triples(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::eval::ExtractedTriple>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::query::debug_triples(conn, &space))
            .await
            .map(|triples| {
                triples
                    .into_iter()
                    .map(|(p, s, o)| oxibrain_core::eval::ExtractedTriple {
                        predicate: p,
                        subject_surface: s,
                        object_surface: o,
                    })
                    .collect()
            })
    }
}
