//! Ingestion methods (M10 10.10). Extracted from lib.rs to keep the facade
//! under 1,000 LOC. The methods here are `pub(crate)` impl blocks on `Brain`;
//! the facade wraps them with 1-line delegations.

use super::{Brain, CaptureOutcome};
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
        let ingested_at = self.clock.now();
        let space = space.to_string();
        let path = path.to_string();
        self.write(move |conn| {
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
            Ok(ep.id)
        })
        .await
    }

    /// Ingest an episode and index it for lexical search. Returns the episode
    /// id. Queue-less since v11: extraction runs inline or via the uncached
    /// backlog, so there is no job to enqueue.
    pub(crate) async fn ingest_impl(
        &self,
        space: &str,
        content: String,
        source: SourceRef,
        trust: TrustTier,
        _extractor_id: &str,
    ) -> Result<String, BrainError> {
        let now = self.clock.now();
        let space = space.to_string();
        self.write(move |conn| {
            oxibrain_store::extraction::ingest_episode(conn, &space, &content, source, trust, now)
        })
        .await
    }

    /// Ingest an episode with event-identity attachment. Returns the episode id.
    /// `trust` is the server-evaluated trust tier for this episode.
    pub(crate) async fn ingest_event_impl(
        &self,
        space: &str,
        content: String,
        source: SourceRef,
        trust: TrustTier,
        attachment: Option<oxibrain_store::ledger::IngestAttachment>,
        _extractor_id: &str,
    ) -> Result<String, BrainError> {
        let now = self.clock.now();
        let space = space.to_string();
        self.write(move |conn| {
            oxibrain_store::extraction::ingest_event(
                conn,
                &space,
                &content,
                source,
                trust,
                attachment.as_ref(),
                now,
            )
        })
        .await
    }

    /// Ensure a source is registered. Returns its id. Idempotent.
    pub(crate) async fn ensure_source_impl(
        &self,
        space: &str,
        name: &str,
        kind: &str,
        mode: &str,
    ) -> Result<String, BrainError> {
        let now = self.clock.now();
        let space = space.to_string();
        let name = name.to_string();
        let kind = kind.to_string();
        let mode = mode.to_string();
        self.write(move |conn| {
            let src_id = oxibrain_core::source_id(&space, &name);
            let row = oxibrain_store::ledger::SourceRow {
                id: src_id.clone(),
                space: space.clone(),
                name,
                kind,
                mode,
                claims_json: "{}".into(),
                created_at: now,
            };
            oxibrain_store::ledger::insert_source(conn, &row)?;
            Ok(src_id)
        })
        .await
    }

    /// `remember` is the unified capture entry for the agent path
    /// (`oxibrain remember --space X < body`, two-plane §9):
    ///
    /// 1. Append a primary note episode (one short write op).
    /// 2. Call the LLM **outside** the write transaction and project the
    ///    validated claims (a second short write op).
    /// 3. Any step-2 failure — no LLM port, provider error, validation
    ///    failure — leaves the episode on the memory-plane backlog and
    ///    returns [`CaptureOutcome::CapturedPending`] so the caller can
    ///    surface "captured, not extracted" and later run
    ///    [`Brain::extract_uncached`](Self::extract_uncached).
    pub async fn remember(
        &self,
        space: &str,
        path: String,
        content: String,
        occurred_at: Timestamp,
    ) -> Result<CaptureOutcome, BrainError> {
        // The episode must land in a real space row (FK); resolve by name so
        // callers may pass either the display name or the id.
        let space_id = self.ensure_space(space).await?;
        let episode_id = self
            .ingest_note_impl(&space_id, &path, content, occurred_at)
            .await?;

        let config = self.extractor_config().clone();
        match self.extract_one(&space_id, &episode_id, &config).await {
            Ok(summary) => Ok(CaptureOutcome::Captured {
                episode_id,
                extracted: summary.extracted,
            }),
            Err(_) => {
                let pending = self
                    .pending_extraction_stats()
                    .await
                    .map(|s| s.count)
                    .unwrap_or(1);
                Ok(CaptureOutcome::CapturedPending {
                    episode_id,
                    pending: pending.max(1),
                })
            }
        }
    }
}
