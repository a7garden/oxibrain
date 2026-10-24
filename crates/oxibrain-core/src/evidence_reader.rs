//! A consumer-side delivery contract. Validation is not semantic entailment.
use crate::evidence::{EvidenceResult, SourceCoordinate};
use oxibrain_ports::BrainError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderEvidence {
    pub id: String,
    pub text: String,
    pub role: String,
    pub source: Option<SourceCoordinate>,
}

/// Bind response-scoped IDs to exact delivered text and full provenance.
pub fn from_result(result: &EvidenceResult) -> Result<Vec<ReaderEvidence>, BrainError> {
    result
        .evidence
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let text = result
                .context
                .layers
                .get(c.layer_index)
                .and_then(|l| l.text.get(c.rendered_text_start..c.rendered_text_end))
                .filter(|_| c.rendered_text_end > c.rendered_text_start)
                .ok_or_else(|| BrainError::Invalid("evidence text offsets unavailable".into()))?;
            Ok(ReaderEvidence {
                id: if c.citation_id.is_empty() {
                    format!("E{}", i + 1)
                } else {
                    c.citation_id.clone()
                },
                text: text.to_owned(),
                role: c.role.clone(),
                source: Some(c.source.clone()),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderAction {
    Answer,
    Conditional,
    Clarify,
    Abstain,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderQuote {
    pub id: String,
    pub quote: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReaderReply {
    pub action: ReaderAction,
    pub answer: String,
    pub conditions: Vec<String>,
    pub clarification: String,
    pub citations: Vec<ReaderQuote>,
}
/// Reject invented IDs/quotes and internally inconsistent actions. Conditions
/// are verbatim so they cannot silently become weaker paraphrases. This only
/// verifies delivery/structure: a model may still omit relevant evidence or draw
/// a conclusion its quotes do not entail. Consumers must not call this proof.
pub fn validate_delivery(
    reply: &ReaderReply,
    evidence: &[ReaderEvidence],
) -> Result<(), BrainError> {
    let invalid = || BrainError::Model("reader reply violates evidence delivery contract".into());
    for citation in &reply.citations {
        if citation.quote.trim().is_empty()
            || !evidence
                .iter()
                .any(|e| e.id == citation.id && e.text.contains(&citation.quote))
        {
            return Err(invalid());
        }
    }
    if reply.conditions.iter().any(|condition| {
        condition.trim().is_empty() || !reply.citations.iter().any(|c| c.quote.contains(condition))
    }) {
        return Err(invalid());
    }
    match reply.action {
        ReaderAction::Answer | ReaderAction::Conditional => {
            if reply.answer.trim().is_empty()
                || reply.citations.is_empty()
                || !reply.clarification.is_empty()
            {
                return Err(invalid());
            }
            if (reply.action == ReaderAction::Conditional) != !reply.conditions.is_empty() {
                return Err(invalid());
            }
        }
        ReaderAction::Clarify => {
            if reply.clarification.trim().is_empty()
                || !reply.answer.is_empty()
                || reply.citations.is_empty()
            {
                return Err(invalid());
            }
        }
        ReaderAction::Abstain => {
            if !reply.answer.is_empty()
                || !reply.conditions.is_empty()
                || !reply.clarification.is_empty()
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
