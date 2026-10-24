//! Pure deterministic selection. Every considered unit receives a terminal decision.
use crate::context::{ContextBudget, ContextLayer, ContextResult, LayerKind, render_context};
use crate::evidence::{
    EvidenceCitation, EvidenceResult, EvidenceUnit, SourceCoordinate, UnitDecision,
};
use oxibrain_ports::TokenizerPort;
use std::fmt::Write;

fn order(k: LayerKind) -> usize {
    match k {
        LayerKind::Profile => 0,
        LayerKind::PinnedFacts => 1,
        LayerKind::HighSalienceBeliefs => 2,
        LayerKind::QueryNeighborhood => 3,
        LayerKind::Documents => 4,
        LayerKind::Summaries => 5,
        LayerKind::RecentEpisodes => 6,
        LayerKind::SessionSummaries => 7,
    }
}

fn already_delivered_contains(candidate: &SourceCoordinate, delivered: &SourceCoordinate) -> bool {
    candidate.reference == delivered.reference
        && candidate.revision == delivered.revision
        && candidate.view == delivered.view
        && candidate.view_digest == delivered.view_digest
        && candidate.decoder == delivered.decoder
        && candidate.document_uuid == delivered.document_uuid
        && delivered.start <= candidate.start
        && candidate.end <= delivered.end
}

fn render(
    selected: &[(&EvidenceUnit, &str, &SourceCoordinate)],
    budget: usize,
    tok: &dyn TokenizerPort,
    previous: Option<&EvidenceResult>,
    compact: bool,
) -> EvidenceResult {
    let mut sorted = selected.to_vec();
    sorted.sort_by_key(|(u, _, _)| order(u.layer));
    let mut layers: Vec<ContextLayer> = Vec::new();
    let mut evidence = Vec::new();
    for (u, text, source) in sorted {
        if layers.last().is_none_or(|l| l.kind != u.layer) {
            layers.push(ContextLayer {
                kind: u.layer,
                text: String::new(),
                estimated_tokens: 0,
                provenance: Vec::new(),
            });
        }
        let layer_index = layers.len() - 1;
        let l = layers.last_mut().expect("layer inserted");
        if !l.text.is_empty() {
            l.text.push('\n');
        }
        let start = l.text.len();
        let citation_id = if compact {
            format!("E{}", evidence.len() + 1)
        } else {
            String::new()
        };
        if compact {
            write!(
                l.text,
                "[{}; role={}; excerpt={}] {}",
                citation_id,
                u.role,
                source.start != u.source.start || source.end != u.source.end,
                text
            )
            .expect("writing to a String cannot fail");
        } else {
            write!(
                l.text,
                "[{}; role={}; view={}; bytes={}..{}; window={}..{}; uuid={}; excerpt={}] {}",
                source.reference,
                u.role,
                source.view,
                source.start,
                source.end,
                u.source.start,
                u.source.end,
                source.document_uuid.as_deref().unwrap_or(""),
                source.start != u.source.start || source.end != u.source.end,
                text
            )
            .expect("writing to a String cannot fail");
            if !u.support.is_empty() {
                l.text.push_str("\n  sources: ");
                for (index, support) in u.support.iter().enumerate() {
                    if index > 0 {
                        l.text.push_str(", ");
                    }
                    l.text.push_str(support);
                }
            }
        }
        let end = l.text.len();
        let rendered_text_end = if compact {
            end
        } else {
            end - if u.support.is_empty() {
                0
            } else {
                "\n  sources: ".len()
                    + u.support.iter().map(String::len).sum::<usize>()
                    + 2 * u.support.len().saturating_sub(1)
            }
        };
        let rendered_text_start = rendered_text_end - text.len();
        l.provenance.push(source.reference.clone());
        evidence.push(EvidenceCitation {
            citation_id,
            unit_id: u.id.clone(),
            layer_index,
            rendered_start: start,
            rendered_end: end,
            rendered_text_start,
            rendered_text_end,
            source: source.clone(),
            role: u.role.clone(),
            support: u.support.clone(),
            shortened: text != u.text,
            omitted_ranges: [(u.source.start, source.start), (source.end, u.source.end)]
                .into_iter()
                .filter(|(a, b)| a < b)
                .collect(),
        });
    }
    for l in &mut layers {
        l.estimated_tokens = previous
            .and_then(|p| {
                p.context
                    .layers
                    .iter()
                    .find(|old| old.kind == l.kind && old.text == l.text)
            })
            .map_or_else(|| tok.count(&l.text), |old| old.estimated_tokens);
    }
    let mut context = ContextResult {
        layers,
        total_tokens: 0,
        budget: ContextBudget { max_tokens: budget },
        truncated: false,
    };
    context.total_tokens = if context.layers.is_empty() {
        0
    } else if context.layers.len() == 1 {
        // The canonical context is byte-identical to its only layer.
        context.layers[0].estimated_tokens
    } else {
        tok.count(&render_context(&context))
    };
    EvidenceResult {
        context,
        evidence,
        decisions: Vec::new(),
    }
}

pub fn pack_evidence(
    units: &[EvidenceUnit],
    budget: usize,
    tok: &dyn TokenizerPort,
) -> EvidenceResult {
    pack_evidence_with_policy(
        units,
        budget,
        tok,
        &crate::read_scope::ReadPolicy::default(),
    )
}

pub fn pack_evidence_with_policy(
    units: &[EvidenceUnit],
    budget: usize,
    tok: &dyn TokenizerPort,
    policy: &crate::read_scope::ReadPolicy,
) -> EvidenceResult {
    let mut selected = Vec::new();
    let mut accepted = render(&[], budget, tok, None, policy.compact_evidence);
    let mut decisions = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // Profile has a bounded priority floor. Query units alternate planes without
    // comparing unrelated score scales; order within each plane is caller-supplied.
    let profiles: Vec<_> = units
        .iter()
        .filter(|u| u.layer == LayerKind::Profile)
        .collect();
    let mut source = Vec::new();
    let mut claims = Vec::new();
    let mut summaries = Vec::new();
    for unit in units.iter().filter(|u| {
        u.layer != LayerKind::Profile
            && u.layer != LayerKind::Documents
            && u.role != "recency_observation"
    }) {
        match unit.layer {
            LayerKind::Summaries => summaries.push(unit),
            LayerKind::RecentEpisodes | LayerKind::SessionSummaries => source.push(unit),
            _ => claims.push(unit),
        }
    }
    // Independent memory channels must get a chance before any one fills the
    // whole budget. Preserve each channel's relevance order and all bundles.
    let mut memory = Vec::new();
    for i in 0..source.len().max(claims.len()).max(summaries.len()) {
        if let Some(u) = source.get(i) {
            memory.push(*u);
        }
        if let Some(u) = summaries.get(i) {
            memory.push(*u);
        }
        if let Some(u) = claims.get(i) {
            memory.push(*u);
        }
    }
    let documents: Vec<_> = units
        .iter()
        .filter(|u| u.layer == LayerKind::Documents)
        .collect();
    let mut ordered = profiles;
    for i in 0..memory.len().max(documents.len()) {
        if let Some(u) = memory.get(i) {
            ordered.push(*u);
        }
        if let Some(u) = documents.get(i) {
            ordered.push(*u);
        }
    }
    ordered.extend(units.iter().filter(|u| u.role == "recency_observation"));
    let mut recency_spent = 0;
    for u in ordered {
        let mut reason = "budget_excluded";
        if !seen.insert(u.id.clone()) {
            reason = "duplicate_source_range";
        } else if u.role == "summary" && u.support.is_empty() {
            reason = "missing_support";
        } else {
            for (text, source) in std::iter::once((&u.text, &u.source))
                .chain(u.variants.iter().map(|v| (&v.text, &v.source)))
            {
                if selected.iter().any(
                    |(_, _, delivered): &(&EvidenceUnit, &str, &SourceCoordinate)| {
                        already_delivered_contains(source, delivered)
                    },
                ) {
                    reason = "duplicate_source_range";
                    continue;
                }
                let before = accepted.context.total_tokens;
                selected.push((u, text.as_str(), source));
                let candidate = render(
                    &selected,
                    budget,
                    tok,
                    Some(&accepted),
                    policy.compact_evidence,
                );
                let bytes = serde_json::to_vec(&candidate.evidence)
                    .expect("evidence serializes")
                    .len();
                let cost = candidate.context.total_tokens.saturating_sub(before);
                let priority_fits = if u.layer == LayerKind::Profile {
                    candidate.context.total_tokens <= (budget / 4).min(policy.profile_tokens)
                } else if u.role == "recency_observation" {
                    recency_spent + cost <= (budget / 10).min(policy.recency_tokens)
                } else {
                    true
                };
                if candidate.context.total_tokens <= budget
                    && bytes <= policy.metadata_bytes
                    && priority_fits
                {
                    reason = if text == &u.text {
                        "selected"
                    } else {
                        "selected_shortened"
                    };
                    if u.role == "recency_observation" {
                        recency_spent += cost;
                    }
                    accepted = candidate;
                    break;
                }
                if bytes > policy.metadata_bytes {
                    reason = "metadata_budget_excluded";
                }
                selected.pop();
            }
        }
        decisions.push(UnitDecision {
            unit_id: u.id.clone(),
            reason: reason.into(),
        });
    }
    let mut out = accepted;
    out.context.truncated = decisions.iter().any(|d| {
        matches!(
            d.reason.as_str(),
            "budget_excluded" | "metadata_budget_excluded" | "selected_shortened"
        )
    });
    out.decisions = decisions;
    out
}
