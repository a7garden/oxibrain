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
}
