//! Tests for Task 1: event-identity source kinds, trust ordinal, and event id derivation.
//!
//! Covers the new `SourceRef` variants, `TrustTier::ordinal` + `Default`, and the
//! `episode_event_id` / `source_id` / `occurrence_id` derivation functions in
//! `oxibrain_core::id`.

use oxibrain_core::id::{episode_event_id, occurrence_id, source_id};
use oxibrain_core::types::{ContentHash, SourceRef, TrustTier};

#[test]
fn new_source_kinds_roundtrip_through_db_columns() {
    let cases = [
        (
            SourceRef::DocumentRevision {
                uri: "vault://a.md".into(),
            },
            "document_revision",
        ),
        (
            SourceRef::ArtifactEvent {
                uri: "oxios://art/1".into(),
            },
            "artifact_event",
        ),
        (
            SourceRef::WebClip {
                uri: "https://x.test".into(),
            },
            "web_clip",
        ),
        (
            SourceRef::CalendarEvent {
                uri: "oxiline://evt/1".into(),
            },
            "calendar_event",
        ),
    ];
    for (s, kind) in cases {
        let (k, r) = s.db_columns();
        assert_eq!(k, kind);
        assert!(r.is_some());
    }
}

#[test]
fn trust_ordinal_is_total_order() {
    assert!(TrustTier::Trusted.ordinal() < TrustTier::SemiTrusted.ordinal());
    assert!(TrustTier::SemiTrusted.ordinal() < TrustTier::Untrusted.ordinal());
}

#[test]
fn trust_default_is_trusted() {
    assert_eq!(TrustTier::default(), TrustTier::Trusted);
}

#[test]
fn episode_event_id_is_deterministic_and_distinct_from_episode_id() {
    let a = episode_event_id("sp", "src1", "occ1");
    assert_eq!(a, episode_event_id("sp", "src1", "occ1"));
    assert_ne!(a, episode_event_id("sp", "src1", "occ2"));
    assert_ne!(a, episode_event_id("sp", "src2", "occ1"));
    assert_ne!(a, episode_event_id("sp2", "src1", "occ1"));
}

#[test]
fn source_id_is_deterministic_and_name_sensitive() {
    let a = source_id("sp", "oximemo-vault");
    assert_eq!(a, source_id("sp", "oximemo-vault"));
    assert_ne!(a, source_id("sp", "other"));
    assert_ne!(a, source_id("sp2", "oximemo-vault"));
}

#[test]
fn occurrence_id_depends_on_predecessor_not_clock() {
    let ch = ContentHash([7u8; 32]);
    let first = occurrence_id("src1", "notes/a.md", None, &ch);
    let again = occurrence_id("src1", "notes/a.md", None, &ch);
    let child = occurrence_id("src1", "notes/a.md", Some(&first), &ch);
    assert_eq!(first, again, "same inputs must regenerate the same id");
    assert_ne!(
        first, child,
        "predecessor changes identity (A->B->A support)"
    );
}
