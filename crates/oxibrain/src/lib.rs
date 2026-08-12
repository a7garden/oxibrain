//! oxibrain: the public facade. P6 — the engine is a library; every surface is an adapter.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod config;

pub use config::BrainConfig;
pub use oxibrain_core::security::{
    AuditEntry, Capability, CapabilitySet, RedactTarget, RedactionClosure, RedactionResult, Scope,
    TokenInfo,
};
pub use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
pub use oxibrain_ports::{
    BrainError, ClockPort, LlmPort, LlmRequest, LlmResponse, SystemClock, Timestamp,
};

pub use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
pub use oxibrain_store::security::AuditRow;
use oxibrain_store::{StoreHandle, ledger, query, reproject};
use std::sync::Arc;

/// The brain. Embedded mode only in M0 (daemon/transport land in M4).
pub struct Brain {
    handle: Arc<StoreHandle>,
    clock: Arc<dyn ClockPort>,
    llm: Option<Arc<dyn LlmPort>>,
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
        })
    }

    /// Ensure a space exists. Returns its id.
    pub async fn ensure_space(&self, name: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let id = ledger::create_space(conn, &name, now)?;
                let _ = tx.send(id);
                Ok(())
            }))?;
            h.writer.flush()?;
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
        let h = self.handle.clone();
        let ingested_at = self.clock.now();
        let space = space.to_string();
        let path = path.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let mut ep = Episode {
                    id: String::new(),
                    space: space.clone(),
                    seq: 0,
                    content_hash: oxibrain_core::ContentHash([0u8; 32]),
                    content,
                    source: SourceRef::Note { path },
                    trust: TrustTier::Trusted,
                    kind: EpisodeKind::Primary,
                    occurred_at,
                    ingested_at,
                    redacted_at: None,
                };
                ledger::insert_episode(conn, &mut ep)?;
                let _ = tx.send(ep.id);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ingest_note channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn get_episode(&self, id: &str) -> Result<Option<Episode>, BrainError> {
        let h = self.handle.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || h.readers.read(|conn| ledger::get_episode(conn, &id)))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn episode_count(&self) -> Result<i64, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || h.readers.read(ledger::episode_count))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Drop and rebuild the entire projection from the ledger.
    pub async fn reproject(&self) -> Result<(), BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                reproject::reproject(conn)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("reproject channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Declare a statement, merge, or retraction. Returns the episode id.
    pub async fn declare(&self, space: &str, decl: Declaration) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let ep_id = oxibrain_store::project::project_declaration(conn, &space, &decl, now)?;
                let _ = tx.send(ep_id);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("declare channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Current beliefs for an entity (follows merge chain).
    pub async fn beliefs(
        &self,
        space: &str,
        entity_id: &str,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| query::beliefs_for_entity(conn, &space, &entity_id))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Beliefs as of a valid-time point.
    pub async fn beliefs_as_of(
        &self,
        space: &str,
        entity_id: &str,
        valid_at: oxibrain_ports::Timestamp,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| query::beliefs_as_of(conn, &space, &entity_id, Some(valid_at), None))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// All contradicted statements in a space.
    pub async fn contradictions(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::Statement>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| query::contradictions(conn, &space))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Hybrid (or mode-specific) query. Returns ranked results with provenance.
    pub async fn query(
        &self,
        q: oxibrain_core::retrieval::Query,
    ) -> Result<oxibrain_core::retrieval::RankingResult, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || h.readers.read(|conn| query::hybrid_query(conn, &q)))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Rebuild all indexes for a space (FTS5, TF-IDF, salience). Runs on the
    /// writer actor so callers never see a half-rebuilt index.
    pub async fn rebuild_indexes(&self, space: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                oxibrain_store::index_ops::rebuild_indexes(conn, &space)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
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
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| query::traverse(conn, &space, &spec))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Look up the entity_id for a surface form + type within a space. Returns
    /// `None` if the entity hasn't been declared yet.
    pub async fn resolve_entity_id(
        &self,
        space: &str,
        ty: &str,
        surface: &str,
    ) -> Result<Option<String>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let ty = ty.to_string();
        let surface = surface.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| query::resolve_entity_id(conn, &space, &ty, &surface))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
    pub async fn timeline(
        &self,
        space: &str,
        entity_id: &str,
        from: Option<oxibrain_ports::Timestamp>,
        to: Option<oxibrain_ports::Timestamp>,
    ) -> Result<Vec<oxibrain_store::timeline::TimelineEntry>, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::timeline::timeline(conn, &space, &entity_id, from, to))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn diff(
        &self,
        space: &str,
        entity_id: &str,
        at_a: oxibrain_ports::Timestamp,
        at_b: oxibrain_ports::Timestamp,
    ) -> Result<oxibrain_store::timeline::DiffResult, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::timeline::diff(conn, &space, &entity_id, at_a, at_b))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn why(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<oxibrain_store::explain::ExplainBlock, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::explain::why(conn, &space, &statement_id))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
    /// Snapshot the index tables (FTS5, TF-IDF, communities) for a space into
    /// a deterministic string. Used by determinism tests.
    pub async fn snapshot_indexes(&self, space: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| oxibrain_store::index_ops::snapshot_indexes(conn, &space))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn rebuild_communities(&self, space: &str) -> Result<(), BrainError> {
        let h = self.handle.clone();
        let space = space.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                oxibrain_store::communities::rebuild_communities(conn, &space)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
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
            h.writer.submit(Box::new(move |conn| {
                let count = oxibrain_store::lifecycle::apply_decay(conn, &space, now, &config)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer.flush()?;
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
            h.writer.submit(Box::new(move |conn| {
                let count = oxibrain_store::lifecycle::compact_episodes(conn, &space, now, 90)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer.flush()?;
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
        let space = space.to_string();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::context::assemble_context(conn, &space, &query, token_budget)
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
        })
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
        let h = self.handle.clone();
        let now = self.clock.now();
        let space = space.to_string();
        let extractor_id = extractor_id.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let ep_id = oxibrain_store::extraction::ingest_and_enqueue(
                    conn,
                    &space,
                    &content,
                    source,
                    trust,
                    &extractor_id,
                    now,
                )?;
                let _ = tx.send(ep_id);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ingest channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
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
            h.writer.submit(Box::new(move |conn| {
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
            h.writer.flush()?;
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
            h.writer.submit(Box::new(move |conn| {
                oxibrain_store::security::revoke_token(conn, &id, now)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
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
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let res = oxibrain_store::redaction::execute_redaction(
                    conn, &target, &reason, &actor, now,
                )?;
                let _ = tx.send(res);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("redact channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
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
            h.writer.submit(Box::new(move |conn| {
                oxibrain_store::export::import_jsonl(conn, &jsonl)?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer.flush()?;
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
    /// backed by the client's model.
    pub async fn extract_one_with(
        &self,
        space: &str,
        episode_id: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
        llm: std::sync::Arc<dyn LlmPort>,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let now = self.clock.now();

        // 1. Read episode content [reader].
        let episode = self
            .get_episode(episode_id)
            .await?
            .ok_or_else(|| BrainError::NotFound(format!("episode {episode_id}")))?;

        // 2. Generate schema + prompt [pure].
        let predicates = oxibrain_core::registry::core_v1();
        let schema = oxibrain_core::extraction::schema_from_registry(predicates);
        let system = oxibrain_core::extraction::build_extraction_prompt(predicates);

        // 3. Call LLM [async, off-actor].
        let req = LlmRequest {
            model: config.model_id.clone(),
            system: Some(system),
            prompt: episode.content.clone(),
            json_schema: Some(schema),
            max_tokens: config.max_tokens,
        };
        let response = llm.complete(req.clone()).await?;

        // 4. Parse + validate [pure].
        let parsed: oxibrain_core::extraction::ExtractionResponse =
            serde_json::from_str(&response.text)
                .map_err(|e| BrainError::Extraction(format!("parse LLM response: {e}")))?;
        let mut result = oxibrain_core::extraction::validate_claims(
            &parsed.claims,
            &episode.content,
            predicates,
        );

        // 5. Repair loop: one retry if invalid claims exist.
        if !result.invalid.is_empty() && config.max_tokens > 0 {
            let errors_summary: Vec<&oxibrain_core::extraction::ValidationError> = result
                .invalid
                .iter()
                .flat_map(|(_, errs)| errs.iter())
                .collect();
            let repair_prompt = format!(
                "{}\n\nPrevious extraction had these errors: {:?}\nPlease re-extract, fixing these issues.",
                episode.content, errors_summary
            );
            let repair_req = LlmRequest {
                prompt: repair_prompt,
                ..req.clone()
            };
            if let Ok(repair_response) = llm.complete(repair_req).await {
                if let Ok(repair_parsed) = serde_json::from_str::<
                    oxibrain_core::extraction::ExtractionResponse,
                >(&repair_response.text)
                {
                    result = oxibrain_core::extraction::validate_claims(
                        &repair_parsed.claims,
                        &episode.content,
                        predicates,
                    );
                }
            }
        }

        let invalid_count = result.invalid.len();
        let raw_response = response.text.clone();
        let extractor_id = config.id();
        let space = space.to_string();
        let episode_id = episode_id.to_string();
        let valid = result.valid.clone();
        let invalid = result.invalid.clone();

        // 6. Project [WriteOp].
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                // Cache the raw response.
                oxibrain_store::extraction::cache_response(
                    conn,
                    &episode_id,
                    &extractor_id,
                    &raw_response,
                    now,
                )?;
                // Project valid claims.
                let n = oxibrain_store::extraction::project_extraction(
                    conn,
                    &space,
                    &episode_id,
                    &extractor_id,
                    &valid,
                    now,
                )?;
                // File invalid claims.
                for (_claim, errors) in &invalid {
                    let errors_json = serde_json::to_string(errors).unwrap_or_else(|_| "[]".into());
                    oxibrain_store::quarantine::record_failure(
                        conn,
                        &episode_id,
                        &extractor_id,
                        &raw_response,
                        &errors_json,
                        now,
                    )?;
                }
                let summary = oxibrain_core::extraction::ExtractSummary {
                    extracted: n,
                    quarantined: invalid_count,
                    episodes_done: 1,
                    episodes_failed: 0,
                };
                let _ = tx.send(summary);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("extract_one channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Process pending extraction jobs in batch. Claims up to
    /// `budget.max_episodes_per_batch` ready jobs and extracts each.
    pub async fn extract_pending(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
        budget: &oxibrain_core::extraction::ExtractionBudget,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let _llm = self.require_llm()?;
        let now = self.clock.now();
        let extractor_id = config.id();

        // 1. Claim jobs.
        let h = self.handle.clone();
        let lease_timeout = budget.lease_timeout_secs;
        let batch_limit = budget.max_episodes_per_batch;
        let jobs = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let _ = oxibrain_store::extraction::reclaim_expired(conn, now);
                let jobs = oxibrain_store::extraction::claim_jobs(
                    conn,
                    &extractor_id,
                    lease_timeout,
                    batch_limit,
                    now,
                )?;
                let _ = tx.send(jobs);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("claim_jobs channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // 2. Process each job via extract_one.
        let mut total = oxibrain_core::extraction::ExtractSummary::default();
        for job in jobs {
            match self.extract_one(space, &job.episode_id, config).await {
                Ok(summary) => {
                    total.extracted += summary.extracted;
                    total.quarantined += summary.quarantined;
                    total.episodes_done += 1;
                    // Complete the job.
                    let h = self.handle.clone();
                    let job_id = job.id.clone();
                    let now = self.clock.now();
                    let _ = tokio::task::spawn_blocking(move || {
                        let (tx, rx) = std::sync::mpsc::channel();
                        let _ = h.writer.submit(Box::new(move |conn| {
                            let _ = tx
                                .send(oxibrain_store::extraction::complete_job(conn, &job_id, now));
                            Ok(())
                        }));
                        let _ = h.writer.flush();
                        rx.recv()
                    })
                    .await;
                }
                Err(e) => {
                    total.episodes_failed += 1;
                    // Fail the job.
                    let h = self.handle.clone();
                    let job_id = job.id.clone();
                    let now = self.clock.now();
                    let max_attempts = budget.max_repair_attempts + 1;
                    let _ = tokio::task::spawn_blocking(move || {
                        let (tx, rx) = std::sync::mpsc::channel();
                        let _ = h.writer.submit(Box::new(move |conn| {
                            let _ = tx.send(oxibrain_store::extraction::fail_job(
                                conn,
                                &job_id,
                                &e.to_string(),
                                max_attempts,
                                now,
                            ));
                            Ok(())
                        }));
                        let _ = h.writer.flush();
                        rx.recv()
                    })
                    .await;
                }
            }
        }
        Ok(total)
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

    /// Re-extract all primary episodes with a new extractor config.
    /// Old cache entries are preserved (different extractor_id = different PK).
    pub async fn reextract(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let _llm = self.require_llm()?;
        let h = self.handle.clone();
        let space = space.to_string();
        let query_space = space.clone();
        let extractor_id = config.id();

        // Find primary episodes that don't have a cache entry for this extractor.
        let episode_ids = tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::extraction::uncached_episodes(conn, &query_space, &extractor_id)
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // Extract each.
        let mut total = oxibrain_core::extraction::ExtractSummary::default();
        for ep_id in episode_ids {
            match self.extract_one(&space, &ep_id, config).await {
                Ok(s) => {
                    total.extracted += s.extracted;
                    total.quarantined += s.quarantined;
                    total.episodes_done += 1;
                }
                Err(_) => {
                    total.episodes_failed += 1;
                }
            }
        }
        Ok(total)
    }

    /// Consolidate related episodes into Derived episodes with cached summaries (§10).
    /// Clusters episodes by shared entities → LLM summarize → Derived episode.
    pub async fn consolidate(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<Vec<String>, BrainError> {
        let llm = self.require_llm()?.clone();
        let now = self.clock.now();
        let h = self.handle.clone();
        let space_owned = space.to_string();
        let extractor_id = config.id();

        // 1. Read episode clusters [reader].
        let clusters = tokio::task::spawn_blocking({
            let h = h.clone();
            let space_owned = space_owned.clone();
            move || {
                h.readers.read(|conn| {
                    oxibrain_store::consolidation::find_episode_clusters(conn, &space_owned)
                })
            }
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // 2. For each cluster: check cache, call LLM if miss.
        let mut summaries: Vec<(Vec<String>, String)> = Vec::new();
        for cluster in clusters {
            let episode_ids = cluster.episode_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_member_set(&episode_ids);
            let cached = tokio::task::spawn_blocking({
                let h = h.clone();
                let extractor_id = extractor_id.clone();
                move || {
                    h.readers.read(|conn| {
                        oxibrain_store::consolidation::get_cached_summary(
                            conn,
                            "consolidation",
                            &member_hash,
                            &extractor_id,
                        )
                    })
                }
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

            if let Some(text) = cached {
                summaries.push((episode_ids.clone(), text));
            } else {
                // Build prompt and call LLM.
                let prompt = tokio::task::spawn_blocking({
                    let h = h.clone();
                    let space_owned = space_owned.clone();
                    let prompt_ids = episode_ids.clone();
                    let prompt_shared = cluster.shared_entities.clone();
                    move || {
                        h.readers.read(|conn| {
                            oxibrain_store::consolidation::build_consolidation_prompt(
                                conn,
                                &space_owned,
                                &oxibrain_store::consolidation::EpisodeCluster {
                                    episode_ids: prompt_ids,
                                    shared_entities: prompt_shared,
                                },
                            )
                        })
                    }
                })
                .await
                .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

                let response = llm
                    .complete(LlmRequest {
                        model: config.model_id.clone(),
                        system: Some("Summarize related episodes concisely.".into()),
                        prompt,
                        json_schema: None,
                        max_tokens: config.max_tokens,
                    })
                    .await?;
                summaries.push((episode_ids, response.text));
            }
        }

        // 3. Write Derived episodes + cache [WriteOp].
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let mut ids = Vec::new();
                for (episode_ids, text) in &summaries {
                    let member_hash = oxibrain_store::consolidation::hash_member_set(episode_ids);
                    oxibrain_store::consolidation::cache_summary(
                        conn,
                        "consolidation",
                        &member_hash,
                        &extractor_id,
                        text,
                        now,
                    )?;
                    let id = oxibrain_store::consolidation::write_derived_episode(
                        conn,
                        &space_owned,
                        text,
                        episode_ids,
                        now,
                    )?;
                    ids.push(id);
                }
                let _ = tx.send(ids);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("consolidate channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Generate community summary text as cached Derived episodes (§9.4, §5.3).
    pub async fn summarize_communities(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<usize, BrainError> {
        let llm = self.require_llm()?.clone();
        let now = self.clock.now();
        let h = self.handle.clone();
        let space_owned = space.to_string();
        let extractor_id = config.id();

        // 1. Read community groups [reader].
        let groups = tokio::task::spawn_blocking({
            let h = h.clone();
            let space_owned = space_owned.clone();
            move || {
                h.readers.read(|conn| {
                    oxibrain_store::consolidation::load_community_entities(conn, &space_owned)
                })
            }
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // 2. For each group: check cache, call LLM if miss.
        let mut summaries: Vec<(Vec<String>, String)> = Vec::new();
        for group in groups {
            let entity_ids = group.entity_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_member_set(&group.entity_ids);
            let cached = tokio::task::spawn_blocking({
                let h = h.clone();
                let extractor_id = extractor_id.clone();
                move || {
                    h.readers.read(|conn| {
                        oxibrain_store::consolidation::get_cached_summary(
                            conn,
                            "community",
                            &member_hash,
                            &extractor_id,
                        )
                    })
                }
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

            if cached.is_some() {
                continue; // cache hit
            }

            // Build prompt and call LLM.
            let prompt = tokio::task::spawn_blocking({
                let h = h.clone();
                let space_owned = space_owned.clone();
                move || {
                    h.readers.read(|conn| {
                        oxibrain_store::consolidation::build_community_prompt(
                            conn,
                            &space_owned,
                            &group,
                        )
                    })
                }
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

            let response = llm
                .complete(LlmRequest {
                    model: config.model_id.clone(),
                    system: Some("Summarize the themes among these entities.".into()),
                    prompt,
                    json_schema: None,
                    max_tokens: config.max_tokens,
                })
                .await?;
            summaries.push((entity_ids, response.text));
        }

        // 3. Write Derived episodes + cache [WriteOp].
        let count = summaries.len();
        if count > 0 {
            tokio::task::spawn_blocking(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                h.writer.submit(Box::new(move |conn| {
                    for (entity_ids, text) in &summaries {
                        let member_hash =
                            oxibrain_store::consolidation::hash_member_set(entity_ids);
                        oxibrain_store::consolidation::cache_summary(
                            conn,
                            "community",
                            &member_hash,
                            &extractor_id,
                            text,
                            now,
                        )?;
                        // Write as a Derived episode linking to episodes mentioning these entities.
                        oxibrain_store::consolidation::write_derived_episode(
                            conn,
                            &space_owned,
                            text,
                            &[],
                            now,
                        )?;
                    }
                    let _ = tx.send(());
                    Ok(())
                }))?;
                h.writer.flush()?;
                rx.recv().map_err(|_| {
                    BrainError::Storage("summarize_communities channel dropped".into())
                })
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        }
        Ok(count)
    }

    /// Extract all (predicate, subject_surface, object_surface) triples from a
    /// space's current projection. Used by the eval suite and CLI `eval` command.
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
