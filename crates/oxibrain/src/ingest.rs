//! Ingestion methods (M10 10.10). Extracted from lib.rs to keep the facade
//! under 1,000 LOC. The methods here are `pub(crate)` impl blocks on `Brain`;
//! the facade wraps them with 1-line delegations.

use super::Brain;
use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{BrainError, Timestamp};
use oxibrain_store::ledger;

impl Brain {
    /// Ingest a note episode. Returns the episode id (content-derived).
    pub(crate) async fn ingest_note_impl(
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
            h.writer()?.submit(Box::new(move |conn| {
                let mut ep = Episode {
                    id: String::new(),
                    space: space.clone(),
                    seq: 0,
                    content_hash: ContentHash([0u8; 32]),
                    content,
                    source: SourceRef::Note { path },
                    trust: TrustTier::Trusted,
                    kind: EpisodeKind::Primary,
                    occurred_at,
                    ingested_at,
                    redacted_at: None,
                };
                ledger::insert_episode(conn, &mut ep)?;
                oxibrain_store::index_ops::index_episode_fts(conn, &ep.space, &ep.id, &ep.content)?;
                let _ = tx.send(ep.id);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ingest_note channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Ingest an episode and enqueue an extraction job. Returns the episode id.
    pub(crate) async fn ingest_impl(
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
            h.writer()?.submit(Box::new(move |conn| {
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
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("ingest channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
}
