//! oxibrain: the public facade. P6 — the engine is a library; every surface is an adapter.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod compat;
pub mod config;
mod extraction;
mod ingest;
pub mod models;
pub mod pull_plan;
mod render;

pub use config::BrainConfig;

// Read-only boilerplate: wrap a closure to be run on a reader connection,
// shipped to a blocking thread. Capture needed values by move before calling.
macro_rules! read_op {
    ($h:expr, |$conn:ident| $body:expr $(,)?) => {{
        let h = $h.clone();
        tokio::task::spawn_blocking(move || h.readers.read(|$conn| $body))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }};
}
pub use oxibrain_core::security::{
    AuditEntry, Capability, CapabilitySet, RedactTarget, RedactionClosure, RedactionResult, Scope,
    TokenInfo,
};
pub use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
pub use oxibrain_ports::{
    BrainError, CharTokenizer, ClockPort, EmbeddingPort, LlmPort, LlmRequest, LlmResponse,
    SystemClock, Timestamp, TokenizerPort,
};

pub use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
pub use oxibrain_store::security::AuditRow;
use oxibrain_store::{StoreHandle, ledger, project::ResolutionCache, query, reproject};
use std::sync::{Arc, Mutex};

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

/// The brain. Embedded mode only in M0 (daemon/transport land in M4).
pub struct Brain {
    handle: Arc<StoreHandle>,
    clock: Arc<dyn ClockPort>,
    llm: Option<Arc<dyn LlmPort>>,
    tokenizer: Arc<dyn TokenizerPort>,
    /// Optional dense embedder for QueryMode::Dense / hybrid dense channel.
    embedder: Option<Arc<dyn EmbeddingPort>>,
    /// Persistent resolution cache: amortises the O(N) LSH index build across
    /// incremental `declare` / `extract` calls. Updated incrementally via
    /// `insert_key` (O(1)) when a new entity key is added; cleared on `reproject`
    /// and `redact` where the projection is rebuilt or entities are removed.
    cache: Arc<Mutex<ResolutionCache>>,
}
impl Brain {
    pub async fn open(config: BrainConfig) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self {
            handle: Arc::new(store),
            clock: Arc::new(SystemClock),
            llm: None,
            tokenizer: Arc::new(CharTokenizer),
            embedder: None,
            cache: Arc::new(Mutex::new(ResolutionCache::new())),
        })
    }

    pub async fn with_clock(
        config: BrainConfig,
        clock: Arc<dyn ClockPort>,
    ) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self {
            handle: Arc::new(store),
            clock,
            llm: None,
            tokenizer: Arc::new(CharTokenizer),
            embedder: None,
            cache: Arc::new(Mutex::new(ResolutionCache::new())),
        })
    }

    /// Open for read-only access (DESIGN §4.3). No advisory lock, no writer actor.
    /// Can coexist with a running daemon — WAL mode allows concurrent readers.
    /// All write methods return `BrainError::Config("store is read-only")`.
    pub async fn open_ro(config: BrainConfig) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open_ro(&config.dir))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self {
            handle: Arc::new(store),
            clock: Arc::new(SystemClock),
            llm: None,
            tokenizer: Arc::new(CharTokenizer),
            embedder: None,
            cache: Arc::new(Mutex::new(ResolutionCache::new())),
        })
    }

    /// Ensure a space exists. Returns its id.
    pub async fn ensure_space(&self, name: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let id = ledger::create_space(conn, &name, now)?;
                let _ = tx.send(id);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ensure_space channel dropped".into()))
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
        read_op!(self.handle, |conn| ledger::note_hashes_by_path(
            conn, &space
        ))
    }

    pub async fn get_episode(&self, id: &str) -> Result<Option<Episode>, BrainError> {
        let id = id.to_string();
        read_op!(self.handle, |conn| ledger::get_episode(conn, &id))
    }

    pub async fn episode_count(&self) -> Result<i64, BrainError> {
        read_op!(self.handle, |conn| ledger::episode_count(conn))
    }

    /// Drop and rebuild the entire projection from the ledger. When an
    /// embedder is configured, dense entity vectors are recomputed after the
    /// reproject pass (§7.6, F17): entity texts read via readers, embeddings
    /// computed outside any writer lock, upserts submitted to the writer.
    pub async fn reproject(&self) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let embedder = self.embedder.clone();
        let inner = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
            h.writer()?.submit(Box::new(move |conn| {
                let result = reproject::reproject(conn).map_err(|e| e.to_string());
                let _ = tx.send(result);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            match rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(BrainError::Storage(format!("reproject: {e}"))),
                Err(_) => return Err(BrainError::Storage("reproject channel dropped".into())),
            }

            // Embed after reproject, outside the writer transaction.
            if let Some(emb) = embedder {
                // Phase 1: read entity texts (readers — read-only is fine).
                let items: Vec<(String, String)> = h.readers.read(|conn| {
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
                })?;
                if !items.is_empty() {
                    // Phase 2: compute embeddings outside any writer lock.
                    let text_refs: Vec<&str> = items.iter().map(|(_, t)| t.as_str()).collect();
                    let vectors = emb
                        .embed(&text_refs)
                        .map_err(|e| BrainError::Config(format!("entity embedding: {e}")))?;
                    let with_vectors: Vec<(String, Vec<f32>)> = items
                        .into_iter()
                        .zip(vectors.into_iter())
                        .map(|((id, _), v)| (id, v))
                        .collect();
                    // Phase 3: upsert via the writer (each a short independent write).
                    let (tx2, rx2) = std::sync::mpsc::channel();
                    h.writer()?.submit(Box::new(move |conn| {
                        oxibrain_store::index_ops::upsert_entity_embeddings(conn, &with_vectors)?;
                        let _ = tx2.send(());
                        Ok(())
                    }))?;
                    h.writer()?.flush()?;
                    rx2.recv()
                        .map_err(|_| BrainError::Storage("embed upsert channel dropped".into()))?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?;

        // The projection was rebuilt from scratch — cached LSH indices are stale
        // (entity ids, keys may have changed). Clear regardless of success or
        // failure: on failure the projection is in an unknown state.
        self.cache
            .lock()
            .expect("resolution cache poisoned")
            .clear();
        inner
    }

    /// Declare a statement, merge, or retraction. Returns the episode id.
    pub async fn declare(&self, space: &str, decl: Declaration) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let now = self.clock.now();
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                // Persistent resolution cache: the O(N) LSH index build is
                // amortised across calls. New keys update the cache in O(1)
                // via `insert_key`, so there is no per-call rebuild.
                let mut cache = cache.lock().expect("resolution cache poisoned");
                let ep_id = oxibrain_store::project::project_declaration(
                    conn, &space, &decl, now, &mut cache,
                )?;
                let _ = tx.send(ep_id);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("declare channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Current beliefs for an entity (follows merge chain).
    /// Current beliefs for an entity (follows merge chain).
    pub async fn beliefs(
        &self,
        space: &str,
        entity_id: &str,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        read_op!(self.handle, |conn| {
            query::beliefs_for_entity(conn, &space, &entity_id)
        })
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
        read_op!(self.handle, |conn| {
            query::beliefs_as_of(conn, &space, &entity_id, Some(valid_at), None)
        })
    }

    /// All contradicted statements in a space.
    pub async fn contradictions(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::Statement>, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| query::contradictions(conn, &space))
    }
    /// Contradicted statements with surfaces and supporting episodes (UI DTO).
    pub async fn contradiction_details(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::query::ContradictionDetail>, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| {
            oxibrain_store::query::contradiction_details(conn, &space)
        })
    }
    /// Aggregate counts for a space (episodes, entities, statements,
    /// contradicted statements). Used by the `stats` MCP tool and dashboards.
    pub async fn stats(&self, space: &str) -> Result<oxibrain_core::SpaceStats, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| query::space_stats(conn, &space))
    }

    /// Hybrid (or mode-specific) query. Returns ranked results with provenance.
    pub async fn query(
        &self,
        q: oxibrain_core::retrieval::Query,
    ) -> Result<oxibrain_core::retrieval::RankingResult, BrainError> {
        let embedder = self.embedder.clone();
        read_op!(self.handle, |conn| {
            query::hybrid_query(conn, &q, embedder.as_deref())
        })
    }
    /// Hybrid (or mode-specific) query, projected to UI-ready search hits.
    /// Mirrors `query()` but enriches entity targets with surface + type
    /// from the entities / entity_keys tables and drops the rest of the
    /// ranking envelope (dropped, total_candidates, spec) — the UI's
    /// `/ask` page only needs the flat hit list (§14.2 ask surface).
    pub async fn search(
        &self,
        q: oxibrain_core::retrieval::Query,
    ) -> Result<Vec<oxibrain_store::query::SearchResult>, BrainError> {
        let embedder = self.embedder.clone();
        let space = q.space.clone();
        read_op!(self.handle, |conn| {
            let ranking = query::hybrid_query(conn, &q, embedder.as_deref())?;
            query::search_results(conn, &space, &ranking)
        })
    }
    pub async fn rebuild_indexes(&self, space: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                oxibrain_store::index_ops::rebuild_indexes(conn, &space)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("rebuild_indexes channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Bounded subgraph traversal over a space's statement graph.
    pub async fn traverse(
        &self,
        space: &str,
        spec: oxibrain_core::retrieval::TraversalSpec,
    ) -> Result<oxibrain_core::retrieval::TraversalResult, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| query::traverse(conn, &space, &spec))
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
        read_op!(self.handle, |conn| {
            query::resolve_entity_id(conn, &space, &ty, &surface)
        })
    }

    pub async fn list_entities(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_core::Entity>, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| {
            oxibrain_store::knowledge::list_entities(conn, &space, limit)
        })
    }

    /// Entities with canonical surfaces resolved, for the `space://` resource.
    /// One SQL join — no N+1.
    pub async fn list_entity_cards(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_store::knowledge::EntityCard>, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| {
            oxibrain_store::knowledge::list_entity_cards(conn, &space, limit)
        })
    }

    /// List merge records in a space, most recent first.
    /// Used by the `review_merges` MCP tool.
    pub async fn list_merges(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::EntityMerge>, BrainError> {
        let space = space.to_string();
        read_op!(self.handle, |conn| {
            oxibrain_store::knowledge::list_merges(conn, &space)
        })
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
        read_op!(self.handle, |conn| {
            oxibrain_store::timeline::timeline(conn, &space, &entity_id, from, to)
        })
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
        read_op!(self.handle, |conn| {
            oxibrain_store::timeline::diff(conn, &space, &entity_id, at_a, at_b)
        })
    }

    pub async fn why(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<oxibrain_store::explain::ExplainBlock, BrainError> {
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        read_op!(self.handle, |conn| {
            oxibrain_store::explain::why(conn, &space, &statement_id)
        })
    }

    /// Render an entity page (`brief`) as Markdown with followable links
    /// (§14.1, M9 §9.2). Pure fetch + pure render — deterministic.
    pub async fn brief(&self, space: &str, entity_id: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        let data = tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::brief::entity_brief(conn, &space, &entity_id))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
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
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || -> Result<String, BrainError> {
            if let Some(topic) = kind.strip_prefix("topic:") {
                let data = h
                    .readers
                    .read(|conn| b::topic_brief(conn, &space, topic, 50))?;
                Ok(render::render_topic_brief(&data))
            } else {
                // kind == "space"
                let data = h.readers.read(|conn| b::space_brief(conn, &space, 50))?;
                Ok(render::render_space_brief(&data))
            }
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Byte-identical snapshot of the truth half (P1, §5.1).
    pub async fn snapshot_truth(&self, space: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::index_ops::snapshot_truth(conn, &space))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Equivalent snapshot of the ranking half (P1, §5.1).
    pub async fn snapshot_ranking(&self, space: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::index_ops::snapshot_ranking(conn, &space))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn rebuild_communities(&self, space: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                oxibrain_store::communities::rebuild_communities(conn, &space)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("rebuild_communities channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
    pub async fn community_members(
        &self,
        space: &str,
        entity_id: &str,
    ) -> Result<Vec<String>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::communities::community_members(conn, &space, &entity_id)
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn apply_decay(&self, space: &str) -> Result<usize, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let now = self.clock.now();
        let config = oxibrain_core::lifecycle::DecayConfig::default();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let count = oxibrain_store::lifecycle::apply_decay(conn, &space, now, &config)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("apply_decay channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn compact(&self, space: &str) -> Result<usize, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let count = oxibrain_store::lifecycle::compact_episodes(conn, &space, now, 90)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("compact channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn assemble_context(
        &self,
        space: &str,
        query: &str,
        token_budget: usize,
    ) -> Result<oxibrain_core::context::ContextResult, BrainError> {
        let h = self.handle.clone();
        let tokenizer = self.tokenizer.clone();
        let space = space.to_string();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::context::assemble_context(
                    conn,
                    &space,
                    &query,
                    token_budget,
                    None,
                    tokenizer.as_ref(),
                )
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
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
        let h = self.handle.clone();
        let tokenizer = self.tokenizer.clone();
        let space = space.to_string();
        let query = query.to_string();
        let hints = hints.clone();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::context::assemble_context(
                    conn,
                    &space,
                    &query,
                    token_budget,
                    Some(&hints),
                    tokenizer.as_ref(),
                )
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Create a Brain with a custom clock and LLM port.
    pub async fn with_llm(
        config: BrainConfig,
        clock: Arc<dyn ClockPort>,
        llm: Arc<dyn LlmPort>,
    ) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self {
            handle: Arc::new(store),
            clock,
            llm: Some(llm),
            tokenizer: Arc::new(CharTokenizer),
            embedder: None,
            cache: Arc::new(Mutex::new(ResolutionCache::new())),
        })
    }

    /// Create a Brain with a custom clock, LLM port, and tokenizer. The local
    /// model path (§7.5) passes the model's own tokenizer so token budgets
    /// are counted, not estimated.
    pub async fn with_llm_and_tokenizer(
        config: BrainConfig,
        clock: Arc<dyn ClockPort>,
        llm: Arc<dyn LlmPort>,
        tokenizer: Arc<dyn TokenizerPort>,
    ) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self {
            handle: Arc::new(store),
            clock,
            llm: Some(llm),
            tokenizer,
            embedder: None,
            cache: Arc::new(Mutex::new(ResolutionCache::new())),
        })
    }

    /// Attach a dense embedder for QueryMode::Dense / hybrid dense channel (§7.6).
    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingPort>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Returns the configured embedder, or None.
    pub fn embedder(&self) -> Option<&Arc<dyn EmbeddingPort>> {
        self.embedder.as_ref()
    }

    /// Returns the configured LLM port, or an error if none.
    fn require_llm(&self) -> Result<&Arc<dyn LlmPort>, BrainError> {
        self.llm
            .as_ref()
            .ok_or_else(|| BrainError::Config("no LLM port configured".into()))
    }

    /// Ingest an episode and enqueue an extraction job. Returns the episode id.
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

    /// Issue a token. Returns (TokenInfo, secret). The secret is shown once.
    pub async fn issue_token(
        &self,
        scope: &Scope,
        issued_by: &str,
        label: Option<&str>,
    ) -> Result<(TokenInfo, String), BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let scope = scope.clone();
        let issued_by = issued_by.to_string();
        let label = label.map(String::from);
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let res = oxibrain_store::security::issue_token(
                    conn,
                    &scope,
                    &issued_by,
                    label.as_deref(),
                    now,
                )?;
                let _ = tx.send(res);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("issue_token channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Verify a token by its secret. Returns the scope if valid and not
    /// expired/revoked.
    pub async fn verify_token(&self, secret: &str) -> Result<Option<Scope>, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let secret = secret.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::security::verify_token(conn, &secret, now))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Revoke a token by id.
    pub async fn revoke_token(&self, id: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                oxibrain_store::security::revoke_token(conn, &id, now)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("revoke_token channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// List all tokens (active and revoked).
    pub async fn list_tokens(&self) -> Result<Vec<TokenInfo>, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || h.readers.read(oxibrain_store::security::list_tokens))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// List recent audit entries, most recent first.
    pub async fn audit_log(&self, limit: Option<i64>) -> Result<Vec<AuditRow>, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::security::list_audit(conn, limit))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Resolve the closure of objects affected by redacting `target`. Does NOT
    /// modify the store — safe for `--dry-run`.
    pub async fn redact_dry_run(
        &self,
        target: &RedactTarget,
    ) -> Result<RedactionClosure, BrainError> {
        let h = self.handle.clone();
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::redaction::resolve_closure(conn, &target))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Execute redaction. Writes audit + redactions record FIRST, then
    /// tombstones and deletes. Returns what was affected.
    pub async fn redact(
        &self,
        target: &RedactTarget,
        reason: &str,
        actor: &str,
    ) -> Result<RedactionResult, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let target = target.clone();
        let reason = reason.to_string();
        let actor = actor.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let res = oxibrain_store::redaction::execute_redaction(
                    conn, &target, &reason, &actor, now,
                )?;
                let _ = tx.send(res);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("redact channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?;

        // Redaction may have deleted entities — cached LSH indices are stale.
        // Only clear on success: on failure, entities weren't changed.
        if result.is_ok() {
            self.cache
                .lock()
                .expect("resolution cache poisoned")
                .clear();
        }
        result
    }

    /// Export all durable tables as a JSONL string.
    pub async fn export_jsonl(&self) -> Result<String, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || h.readers.read(oxibrain_store::export::export_jsonl))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Import JSONL into the store. Assumes the store is fresh (tables empty).
    pub async fn import_jsonl(&self, jsonl: String) -> Result<(), BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                oxibrain_store::export::import_jsonl(conn, &jsonl)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("import_jsonl channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

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

    /// Process pending extraction jobs in batch. Claims up to
    /// `budget.max_episodes_per_batch` ready jobs and extracts each.
    pub async fn extract_pending(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
        budget: &oxibrain_core::extraction::ExtractionBudget,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        self.extract_pending_impl(space, config, budget).await
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

    /// Query job queue status (counts by state).
    pub async fn job_status(&self) -> Result<Vec<(String, usize)>, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                let jobs = oxibrain_store::extraction::list_jobs(conn, None)?;
                let mut counts: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for job in jobs {
                    *counts.entry(job.state.as_str().to_string()).or_default() += 1;
                }
                Ok(counts.into_iter().collect())
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
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

    /// Render statements by id as `id | subject predicate object` text.
    /// Used by the gate runner to score ranked items against answers.
    pub async fn render_statements(
        &self,
        space: &str,
        ids: &[String],
    ) -> Result<Vec<String>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let ids = ids.to_vec();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::query::render_statements(conn, &space, &ids))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
    /// Look up statement IDs where the given entities are subject or object.
    pub async fn statements_for_entities(
        &self,
        space: &str,
        entity_ids: &[String],
    ) -> Result<Vec<String>, BrainError> {
        let (h, space, eids) = (self.handle.clone(), space.to_string(), entity_ids.to_vec());
        read_op!(h, |conn| oxibrain_store::query::statements_for_entities(
            conn, &space, &eids
        ))
    }

    /// Extract all triples from a space's current projection.
    pub async fn debug_triples(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::eval::ExtractedTriple>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::query::debug_triples(conn, &space))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
        .map(|t| {
            t.into_iter()
                .map(|(p, s, o)| oxibrain_core::eval::ExtractedTriple {
                    predicate: p,
                    subject_surface: s,
                    object_surface: o,
                })
                .collect()
        })
    }
}
