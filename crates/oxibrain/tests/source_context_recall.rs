//! Source prefix companions retain the beginning of a selected raw source.
use oxibrain::{Brain, BrainConfig, ReadPolicy, RecallRequest, Timestamp};
use oxibrain_core::context::render_context;
use oxibrain_core::retrieval::SearchPlane;
use oxibrain_ports::{TokenizerPort, tokenizer::TokenizerQuality};
use std::sync::Arc;

struct Bytes;

impl TokenizerPort for Bytes {
    fn count(&self, text: &str) -> usize {
        text.len()
    }

    fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
        text.chars().take(max_tokens).collect()
    }

    fn id(&self) -> &str {
        "source-context-byte-tokenizer"
    }

    fn quality(&self) -> TokenizerQuality {
        TokenizerQuality::Exact
    }
}

fn body() -> String {
    format!(
        "{} TailNeedleKite identifies the release candidate.",
        "Early prerequisite requires the staged artifacts. ".repeat(80)
    )
}

fn request(space: &str, budget: usize) -> RecallRequest {
    let mut request = RecallRequest::new(space, "TailNeedleKite", budget);
    request.planes = [SearchPlane::Memory].into_iter().collect();
    request.require_exact_tokens = true;
    request.hints.skip_beliefs = true;
    request.hints.skip_summaries = true;
    request
}

async fn configured_brain(path: &std::path::Path, source_context_bytes: usize) -> (Brain, String) {
    let policy = ReadPolicy {
        source_context_bytes,
        ..Default::default()
    };
    let brain = Brain::open(BrainConfig::at(path))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes))
        .with_read_policy(policy)
        .unwrap();
    let space = brain.ensure_space("source-context").await.unwrap();
    brain
        .record_session(&space, "late-session", body(), Timestamp(1))
        .await
        .unwrap();
    brain.index_memory().await.unwrap();
    (brain, space)
}

fn prefix_and_hit(result: &oxibrain::RecallResponse) -> (usize, usize) {
    let hit = result
        .data
        .evidence
        .iter()
        .position(|citation| {
            citation.source.reference == "session://late-session"
                && citation.role == "untrusted_observation"
                && citation.source.start > 0
        })
        .expect("tail query hit is delivered");
    let prefix = result
        .data
        .evidence
        .iter()
        .position(|citation| {
            citation.source.reference == "session://late-session"
                && citation.role == "untrusted_observation"
                && citation.source.start == 0
                && citation.source.end <= 768
        })
        .expect("prefix companion is delivered");
    (hit, prefix)
}

#[tokio::test]
async fn tail_hit_gets_one_adjacent_provenance_bound_prefix_with_exact_budgeting() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, space) = configured_brain(dir.path(), 768).await;
    let complete = brain
        .recall_with_request(request(&space, 20_000))
        .await
        .unwrap();
    let (hit_index, prefix_index) = prefix_and_hit(&complete);
    assert_eq!(prefix_index, hit_index + 1);

    let hit = &complete.data.evidence[hit_index];
    let prefix = &complete.data.evidence[prefix_index];
    assert_eq!(prefix.source.reference, hit.source.reference);
    assert_eq!(prefix.source.revision, hit.source.revision);
    assert_eq!(prefix.source.view_digest, hit.source.view_digest);
    assert_eq!(prefix.source.start, 0);
    assert!(prefix.source.end > 0 && prefix.source.end <= 768);
    assert!(render_context(&complete.data.context).contains("Early prerequisite"));
    assert_eq!(complete.meta.scope["limits"]["source_context_bytes"], 768);
    assert_eq!(
        complete.meta.scope["source_context"]["candidate_prefixes"],
        1
    );

    let exact = brain
        .recall_with_request(request(&space, complete.data.context.total_tokens))
        .await
        .unwrap();
    prefix_and_hit(&exact);
    assert!(exact.data.context.total_tokens <= complete.data.context.total_tokens);

    let tight_budget = complete.data.context.total_tokens.saturating_sub(1);
    let tight = brain
        .recall_with_request(request(&space, tight_budget))
        .await
        .unwrap();
    assert!(tight.data.context.total_tokens <= tight_budget);
    assert!(tight.data.decisions.iter().any(|decision| {
        matches!(
            decision.reason.as_str(),
            "budget_excluded" | "selected_shortened"
        )
    }));
}

#[tokio::test]
async fn zero_source_context_bytes_disables_prefix_without_suppressing_query_hit() {
    let dir = tempfile::tempdir().unwrap();
    let (brain, space) = configured_brain(dir.path(), 0).await;
    let result = brain
        .recall_with_request(request(&space, 20_000))
        .await
        .unwrap();

    assert!(result.data.evidence.iter().any(|citation| {
        citation.source.reference == "session://late-session"
            && citation.role == "untrusted_observation"
            && citation.source.start > 0
    }));
    assert!(!result.data.evidence.iter().any(|citation| {
        citation.source.reference == "session://late-session"
            && citation.role == "untrusted_observation"
            && citation.source.start == 0
    }));
    assert_eq!(result.meta.scope["source_context"]["enabled"], false);
    assert_eq!(result.meta.scope["source_context"]["executed"], false);
}
