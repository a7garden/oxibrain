//! Session memory (agent session summaries). Session summaries are Primary
//! episodes on the immutable ledger with `SourceRef::Session` provenance —
//! nothing outside the P1 ledger, everything rebuildable.

use crate::Brain;
use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{BrainError, Timestamp};
use oxibrain_store::ledger;

impl Brain {
    /// Record a session summary as an Untrusted Primary episode with event identity
    /// `(space, "sessions" source, session_id)`. Re-recording the same
    /// session id with identical bytes is idempotent; different bytes are
    /// [`BrainError::Conflict`] (ledger semantics, §4.2).
    pub async fn record_session(
        &self,
        space: &str,
        session_id: &str,
        summary: String,
        occurred_at: Timestamp,
    ) -> Result<String, BrainError> {
        let source_id = self
            .ensure_source(space, "sessions", "session", "push")
            .await?;
        let attachment = ledger::IngestAttachment {
            source_id,
            occurrence_id: session_id.to_string(),
            accepted_at: self.clock.now(),
            principal: "session".into(),
            claims_json: "{}".into(),
        };
        let space = space.to_string();
        let sid = session_id.to_string();
        let ingested_at = self.clock.now();
        self.write(move |conn| {
            let mut ep = Episode {
                id: String::new(),
                space,
                seq: 0,
                content_hash: ContentHash([0u8; 32]),
                content: summary,
                source: SourceRef::Session { session_id: sid },
                trust: TrustTier::Untrusted,
                kind: EpisodeKind::Primary,
                occurred_at,
                ingested_at,
                redacted_at: None,
            };
            ledger::insert_event(conn, &mut ep, Some(&attachment))?;
            oxibrain_store::index_ops::index_episode_fts(conn, &ep.space, &ep.id, &ep.content)?;
            Ok(ep.id)
        })
        .await
    }

    /// List recorded session summaries, newest-first by occurred_at.
    /// `limit` caps the result (caller clamps); `exclude_session_id` drops
    /// the caller's current session.
    pub async fn list_sessions(
        &self,
        space: &str,
        limit: i64,
        exclude_session_id: Option<&str>,
    ) -> Result<Vec<oxibrain_core::pack::SessionSummary>, BrainError> {
        let space = space.to_string();
        let exclude = exclude_session_id.map(str::to_string);
        self.read(move |conn| {
            oxibrain_store::context::list_session_episodes(conn, &space, limit, exclude.as_deref())
        })
        .await
    }
}
