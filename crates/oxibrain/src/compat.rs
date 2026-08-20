//! Compatibility surface test (DESIGN §16.4).
//!
//! This module references every stable `Brain` method. If a method is removed
//! or its signature changes incompatibly, this module fails to compile — the
//! core mechanism of the consumption contract.

#![allow(dead_code, unused_variables)]

use crate::{Brain, BrainConfig};
use oxibrain_ports::{ClockPort, LlmPort};

/// Associated-function checks — verify constructor signatures exist.
fn _check_constructors(
    _config: BrainConfig,
    _clock: std::sync::Arc<dyn ClockPort>,
    _llm: std::sync::Arc<dyn LlmPort>,
) {
    let _ = Brain::open;
    let _ = Brain::with_clock;
    let _ = Brain::with_llm;
}

/// Method-reference checks — verify each method exists on the Brain type.
fn _check_methods(_brain: &Brain) {
    // Ingestion

    let _ = Brain::ensure_space;
    let _ = Brain::lookup_space;
    let _ = Brain::list_spaces;
    let _ = Brain::ingest_note;
    let _ = Brain::ingest;
    let _ = Brain::get_episode;
    let _ = Brain::episode_count;

    // Query
    let _ = Brain::query;
    let _ = Brain::assemble_context;
    let _ = Brain::beliefs;
    let _ = Brain::beliefs_as_of;
    let _ = Brain::contradictions;
    let _ = Brain::traverse;
    let _ = Brain::timeline;
    let _ = Brain::diff;
    let _ = Brain::why;
    let _ = Brain::resolve_entity_id;
    let _ = Brain::list_entities;
    let _ = Brain::list_merges;

    // Mutation
    let _ = Brain::declare;
    let _ = Brain::redact;
    let _ = Brain::redact_dry_run;

    // Lifecycle
    let _ = Brain::reproject;
    let _ = Brain::rebuild_indexes;
    let _ = Brain::rebuild_communities;
    let _ = Brain::apply_decay;
    let _ = Brain::compact;
    let _ = Brain::community_members;
    let _ = Brain::snapshot_truth;
    let _ = Brain::snapshot_ranking;

    // Extraction
    let _ = Brain::extract_one;
    let _ = Brain::extract_one_with;
    let _ = Brain::extract_pending;
    let _ = Brain::reextract;
    let _ = Brain::consolidate;
    let _ = Brain::summarize_communities;
    let _ = Brain::job_status;

    // Security
    let _ = Brain::issue_token;
    let _ = Brain::verify_token;
    let _ = Brain::revoke_token;
    let _ = Brain::list_tokens;
    let _ = Brain::audit_log;

    // Export/Import
    let _ = Brain::export_jsonl;
    let _ = Brain::import_jsonl;

    // Eval/debug
    let _ = Brain::debug_triples;
}

/// Type-reexport checks — verify all stable types are accessible from the
/// crate root.
fn _check_types() {
    fn _assert<T>() {}
    _assert::<Brain>();
    _assert::<BrainConfig>();
    _assert::<crate::Episode>();
    _assert::<crate::EpisodeKind>();
    _assert::<crate::SourceRef>();
    _assert::<crate::TrustTier>();
    _assert::<crate::BrainError>();
    _assert::<crate::Timestamp>();
    _assert::<crate::Capability>();
    _assert::<crate::Scope>();
    _assert::<crate::TokenInfo>();
    _assert::<crate::Declaration>();
    _assert::<crate::EntityRef>();
    _assert::<crate::DeclObject>();
    _assert::<crate::RedactTarget>();
    _assert::<crate::RedactionClosure>();
    _assert::<crate::RedactionResult>();
    _assert::<crate::AuditRow>();
    _assert::<crate::SpaceInfo>();
}

#[cfg(test)]
#[test]
fn compat_surface_compiles() {
    // If this test compiles, every method and type reference above resolved.
}
