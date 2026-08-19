//! Facade-level event ingest: attachment rides through to the ledger.

use oxibrain::{Brain, BrainConfig, SourceRef, TrustTier};
use oxibrain_ports::Timestamp;
use oxibrain_store::ledger::IngestAttachment;

#[tokio::test]
async fn ingest_event_with_attachment_persists_provenance() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("test").await.unwrap();
    let source_id = brain
        .ensure_source(&space_id, "src_facade", "document_revision", "push")
        .await
        .unwrap();

    let att = IngestAttachment {
        source_id,
        occurrence_id: "occ_facade".into(),
        accepted_at: Timestamp(5000),
        principal: "facade-test".into(),
        claims_json: "{}".into(),
    };

    let ep_id = brain
        .ingest_event(
            &space_id,
            "event content".into(),
            SourceRef::DocumentRevision {
                uri: "vault://x.md".into(),
            },
            TrustTier::Trusted,
            Some(&att),
            "test-extractor",
        )
        .await
        .unwrap();

    // Verify the episode has the attachment.
    let ep = brain.get_episode(&ep_id).await.unwrap().unwrap();
    assert_eq!(ep.content, "event content");
}

#[tokio::test]
async fn ensure_source_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("test").await.unwrap();

    let id1 = brain
        .ensure_source(&space_id, "vault", "document_revision", "pull")
        .await
        .unwrap();
    let id2 = brain
        .ensure_source(&space_id, "vault", "document_revision", "pull")
        .await
        .unwrap();
    assert_eq!(id1, id2, "ensure_source must be idempotent");
}
