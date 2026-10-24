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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReaderDeliveryError {
    #[error("citation {index} has an empty quote")]
    EmptyCitationQuote { index: usize },
    #[error("citation {index} names unknown evidence ID {id:?}")]
    UnknownEvidenceId { index: usize, id: String },
    #[error("citation {index} quote is not an exact substring of evidence {id:?}")]
    QuoteNotDelivered { index: usize, id: String },
    #[error("condition {index} is empty")]
    EmptyCondition { index: usize },
    #[error("condition {index} is not an exact substring of a cited quotation")]
    ConditionNotCited { index: usize },
    #[error("action {action:?} requires at least one valid citation")]
    CitationsRequired { action: ReaderAction },
    #[error("action {action:?} requires a non-empty answer")]
    AnswerRequired { action: ReaderAction },
    #[error("action {action:?} requires an empty clarification field")]
    ClarificationMustBeEmpty { action: ReaderAction },
    #[error("answer action cannot include conditions; use conditional action")]
    ConditionsRequireConditional,
    #[error("conditional action requires at least one cited condition")]
    ConditionalRequiresCondition,
    #[error("clarify action requires a focused, non-empty clarification")]
    ClarificationRequired,
    #[error("clarify action requires an empty answer field")]
    AnswerMustBeEmptyForClarify,
    #[error("abstain action requires empty answer, conditions, and clarification fields")]
    AbstentionFieldsMustBeEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("reader delivery has {count} contract violation(s)", count = .errors.len())]
#[serde(transparent)]
pub struct ReaderDeliveryErrors {
    pub errors: Vec<ReaderDeliveryError>,
}

/// Reject invented IDs/quotes and internally inconsistent actions. Conditions
/// are verbatim so they cannot silently become weaker paraphrases. This only
/// verifies delivery/structure: a model may still omit relevant evidence or draw
/// a conclusion its quotes do not entail. Consumers must not call this proof.
pub fn validate_delivery(
    reply: &ReaderReply,
    evidence: &[ReaderEvidence],
) -> Result<(), ReaderDeliveryErrors> {
    let mut errors = Vec::new();
    for (index, citation) in reply.citations.iter().enumerate() {
        if citation.quote.trim().is_empty() {
            errors.push(ReaderDeliveryError::EmptyCitationQuote { index });
            continue;
        }
        let Some(item) = evidence.iter().find(|e| e.id == citation.id) else {
            errors.push(ReaderDeliveryError::UnknownEvidenceId {
                index,
                id: citation.id.clone(),
            });
            continue;
        };
        if !item.text.contains(&citation.quote) {
            errors.push(ReaderDeliveryError::QuoteNotDelivered {
                index,
                id: citation.id.clone(),
            });
        }
    }
    for (index, condition) in reply.conditions.iter().enumerate() {
        if condition.trim().is_empty() {
            errors.push(ReaderDeliveryError::EmptyCondition { index });
        } else if !reply
            .citations
            .iter()
            .any(|citation| citation.quote.contains(condition))
        {
            errors.push(ReaderDeliveryError::ConditionNotCited { index });
        }
    }
    match reply.action {
        ReaderAction::Answer | ReaderAction::Conditional => {
            if reply.answer.trim().is_empty() {
                errors.push(ReaderDeliveryError::AnswerRequired {
                    action: reply.action,
                });
            }
            if reply.citations.is_empty() {
                errors.push(ReaderDeliveryError::CitationsRequired {
                    action: reply.action,
                });
            }
            if !reply.clarification.is_empty() {
                errors.push(ReaderDeliveryError::ClarificationMustBeEmpty {
                    action: reply.action,
                });
            }
            if reply.action == ReaderAction::Answer && !reply.conditions.is_empty() {
                errors.push(ReaderDeliveryError::ConditionsRequireConditional);
            }
            if reply.action == ReaderAction::Conditional && reply.conditions.is_empty() {
                errors.push(ReaderDeliveryError::ConditionalRequiresCondition);
            }
        }
        ReaderAction::Clarify => {
            if reply.clarification.trim().is_empty() {
                errors.push(ReaderDeliveryError::ClarificationRequired);
            }
            if !reply.answer.is_empty() {
                errors.push(ReaderDeliveryError::AnswerMustBeEmptyForClarify);
            }
            if reply.citations.is_empty() {
                errors.push(ReaderDeliveryError::CitationsRequired {
                    action: ReaderAction::Clarify,
                });
            }
        }
        ReaderAction::Abstain => {
            if !reply.answer.is_empty()
                || !reply.conditions.is_empty()
                || !reply.clarification.is_empty()
            {
                errors.push(ReaderDeliveryError::AbstentionFieldsMustBeEmpty);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ReaderDeliveryErrors { errors })
    }
}
