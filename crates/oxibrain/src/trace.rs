//! Trajectory memory (experience memory spec §2, M11). A rendered agent
//! trajectory is a Primary episode with `SourceRef::AgentTrace` provenance
//! and event identity `(space, "agent:<agent>" source, run_id)` — nothing
//! outside the P1 ledger, everything rebuildable.

use crate::Brain;
use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{BrainError, Timestamp};
use oxibrain_store::ledger;

impl Brain {
    /// Record a rendered agent trajectory as a Primary episode with event
    /// identity `(space, "agent:<agent>" source, run_id)`. Replaying the
    /// same run id with identical bytes is idempotent; the same run id with
    /// different bytes is [`BrainError::Conflict`] (ledger semantics, §4.2).
    ///
    /// Trajectories are agent-generated claims, not owner declarations
    /// (v2.21): callers should pass [`TrustTier::Untrusted`] unless a
    /// capability (TrustedIngest) or source policy justifies more. Elevation
    /// for a given agent is a source-policy Declaration on
    /// `agent:<agent>`; the per-agent source row is ensured here
    /// idempotently, exactly like the session source.
    pub async fn record_trace(
        &self,
        space: &str,
        agent: &str,
        run_id: &str,
        content: String,
        occurred_at: Timestamp,
        trust: TrustTier,
    ) -> Result<String, BrainError> {
        if agent.trim().is_empty() {
            return Err(BrainError::Invalid("trace agent must be non-empty".into()));
        }
        if run_id.trim().is_empty() {
            return Err(BrainError::Invalid("trace run_id must be non-empty".into()));
        }
        let source_name = format!("agent:{agent}");
        let source_id = self
            .ensure_source(space, &source_name, "agent", "push")
            .await?;
        let attachment = ledger::IngestAttachment {
            source_id,
            occurrence_id: run_id.to_string(),
            accepted_at: self.clock.now(),
            principal: agent.to_string(),
            claims_json: "{}".into(),
        };
        let space = space.to_string();
        let agent = agent.to_string();
        let ingested_at = self.clock.now();
        self.write(move |conn| {
            let mut ep = Episode {
                id: String::new(),
                space,
                seq: 0,
                content_hash: ContentHash([0u8; 32]),
                content,
                source: SourceRef::AgentTrace { agent },
                trust,
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
}
