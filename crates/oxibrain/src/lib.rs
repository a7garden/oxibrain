//! oxibrain: the public facade. P6 — the engine is a library; every surface is an adapter.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod config;

pub use config::BrainConfig;
pub use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
pub use oxibrain_ports::{BrainError, ClockPort, SystemClock, Timestamp};

pub use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use oxibrain_store::{StoreHandle, ledger, query, reproject};
use std::sync::Arc;

/// The brain. Embedded mode only in M0 (daemon/transport land in M4).
pub struct Brain {
    handle: Arc<StoreHandle>,
    clock: Arc<dyn ClockPort>,
}

impl Brain {
    pub async fn open(config: BrainConfig) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir))
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self {
            handle: Arc::new(store),
            clock: Arc::new(SystemClock),
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
        tokio::task::spawn_blocking(move || {
            h.readers
                .read(|conn| query::hybrid_query(conn, &q))
        })
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
            rx.recv().map_err(|_| {
                BrainError::Storage("rebuild_communities channel dropped".into())
            })
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
                let count =
                    oxibrain_store::lifecycle::compact_episodes(conn, &space, now, 90)?;
                let _ = tx.send(count);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv().map_err(|_| BrainError::Storage("compact channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

}
