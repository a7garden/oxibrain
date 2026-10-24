//! Source coordinates are in the named decoded UTF-8 view, never raw HTML offsets.
use crate::context::{ContextResult, LayerKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceCoordinate {
    pub reference: String,
    pub revision: String,
    pub view: String,
    pub view_digest: Option<String>,
    pub decoder: Option<String>,
    pub start: usize,
    pub end: usize,
    pub document_uuid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceUnit {
    pub id: String,
    pub layer: LayerKind,
    pub text: String,
    pub source: SourceCoordinate,
    pub role: String,
    pub support: Vec<String>,
    pub variants: Vec<EvidenceVariant>,
}

impl EvidenceUnit {
    pub fn new(layer: LayerKind, text: String, source: SourceCoordinate, role: &str) -> Self {
        let identity = serde_json::to_vec(&("source-unit/1", &source, role, layer))
            .expect("source coordinate serializes");
        Self {
            id: blake3::hash(&identity).to_hex().to_string(),
            layer,
            text,
            source,
            role: role.into(),
            support: Vec::new(),
            variants: Vec::new(),
        }
    }
}

/// Assemble a document candidate from one verified decoded view. Search hits
/// remain small; recall expands them to a complete enclosing section and refuses
/// unsafe cross-section fallback ranges.
pub fn document_recall_unit(
    text: &str,
    headings: Option<&[crate::documents::DocumentHeading]>,
    source: SourceCoordinate,
    query: &str,
    short_document_bytes: usize,
) -> Option<EvidenceUnit> {
    let mut units = document_recall_units(text, headings, source, query, short_document_bytes)?;
    (units.len() == 1).then(|| units.remove(0))
}

/// Assemble every complete, query-relevant section represented by one verified
/// decoded-view hit. Multiple anchored peer sections are preserved together.
pub fn document_recall_units(
    text: &str,
    headings: Option<&[crate::documents::DocumentHeading]>,
    source: SourceCoordinate,
    query: &str,
    short_document_bytes: usize,
) -> Option<Vec<EvidenceUnit>> {
    let original = source.start..source.end;
    let ranges = match headings {
        Some(headings) => crate::documents::recall_document_ranges(
            text,
            headings,
            original.clone(),
            query,
            short_document_bytes,
        )?,
        None if original.start == 0 && original.end == text.len() => vec![original.clone()],
        None => return None,
    };
    let Some(headings) = headings else {
        // The source decoder could not provide offsets aligned with this view.
        // A whole-view read is safe; a body-only excerpt is not.
        let range = ranges.into_iter().next()?;
        let mut source = source;
        source.start = range.start;
        source.end = range.end;
        return Some(vec![EvidenceUnit::new(
            LayerKind::Documents,
            text.get(range)?.to_string(),
            source,
            "untrusted_observation",
        )]);
    };

    let mut units = Vec::with_capacity(ranges.len());
    for range in ranges {
        let mut unit_source = source.clone();
        unit_source.start = range.start;
        unit_source.end = range.end;
        let mut unit = EvidenceUnit::new(
            LayerKind::Documents,
            text.get(range.clone())?.to_string(),
            unit_source,
            "untrusted_observation",
        );
        let section = headings
            .iter()
            .any(|h| h.start == range.start && h.end <= range.end);
        let original_is_heading = original.start >= range.start
            && original.end <= range.end
            && headings.iter().any(|h| {
                text.get(h.start..h.end)
                    .is_some_and(|title| title.trim() == text[original.clone()].trim())
            });
        if range == original && headings.is_empty() {
            unit.variants = quotation_variants(&unit.text, &unit.source, query);
        } else if range != original
            && (!section || original_is_heading)
            && range.start <= original.start
            && range.end >= original.end
        {
            // A command without its later applicability condition is not a
            // safe excerpt. Only a heading-only match may stand alone.
            let mut variant_source = unit.source.clone();
            variant_source.start = original.start;
            variant_source.end = original.end;
            unit.variants.push(EvidenceVariant {
                text: text[original.clone()].to_string(),
                source: variant_source,
            });
        }
        units.push(unit);
    }
    Some(units)
}

/// One verified section candidate and its bounded retrieval score.
#[derive(Debug, Clone)]
pub struct DocumentRecallCandidate {
    pub unit: EvidenceUnit,
    pub retrieval_score: f64,
}

/// Keep all independently retrieved sections from one document view. Query
/// match strength orders peers, while fused retrieval score remains the primary
/// cross-document ordering signal. Repeated hits for the same source range are
/// deduplicated by stable unit identity.
pub fn select_document_recall_units(
    query: &str,
    candidates: Vec<DocumentRecallCandidate>,
) -> Vec<EvidenceUnit> {
    let plan = crate::query_candidates::LexicalQueryPlan::new(query);
    let mut groups = std::collections::BTreeMap::<
        (String, String, String, Option<String>),
        Vec<(crate::query_candidates::LexicalRelevance, f64, EvidenceUnit)>,
    >::new();
    for candidate in candidates {
        let unit = candidate.unit;
        let key = (
            unit.source.reference.clone(),
            unit.source.revision.clone(),
            unit.source.view.clone(),
            unit.source.view_digest.clone(),
        );
        groups.entry(key).or_default().push((
            plan.relevance(&unit.text),
            candidate.retrieval_score,
            unit,
        ));
    }

    let mut selected = Vec::<(f64, crate::query_candidates::LexicalRelevance, EvidenceUnit)>::new();
    for (_, mut group) in groups {
        let mut by_id = std::collections::BTreeMap::<
            String,
            (crate::query_candidates::LexicalRelevance, f64, EvidenceUnit),
        >::new();
        for (relevance, score, unit) in group.drain(..) {
            by_id
                .entry(unit.id.clone())
                .and_modify(|existing| {
                    if score > existing.1 {
                        *existing = (relevance, score, unit.clone());
                    }
                })
                .or_insert((relevance, score, unit));
        }
        let mut group: Vec<_> = by_id.into_values().collect();
        group.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| a.2.source.start.cmp(&b.2.source.start))
                .then_with(|| a.2.id.cmp(&b.2.id))
        });
        selected.extend(
            group
                .into_iter()
                .map(|(relevance, score, unit)| (score, relevance, unit)),
        );
    }
    selected.sort_by(|(a_score, a_relevance, a), (b_score, b_relevance, b)| {
        b_score
            .total_cmp(a_score)
            .then_with(|| b_relevance.cmp(a_relevance))
            .then_with(|| a.source.reference.cmp(&b.source.reference))
            .then_with(|| a.source.revision.cmp(&b.source.revision))
            .then_with(|| a.source.view.cmp(&b.source.view))
            .then_with(|| a.source.start.cmp(&b.source.start))
            .then_with(|| a.id.cmp(&b.id))
    });
    selected.into_iter().map(|(_, _, unit)| unit).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceVariant {
    pub text: String,
    pub source: SourceCoordinate,
}

/// Complete quotation variants. Structured beliefs and summaries never use this.
/// Offsets remain in the source view, including case-expanding Unicode scalars.
pub fn quotation_variants(
    text: &str,
    source: &SourceCoordinate,
    query: &str,
) -> Vec<EvidenceVariant> {
    let mut lower = String::new();
    let mut offsets = Vec::new();
    for (original, ch) in text.char_indices() {
        for c in ch.to_lowercase() {
            offsets.push((lower.len(), original));
            lower.push(c);
        }
    }
    let focus = query
        .split_whitespace()
        .filter_map(|q| lower.find(&q.to_lowercase()))
        .filter_map(|at| {
            offsets
                .iter()
                .rev()
                .find(|(i, _)| *i <= at)
                .map(|(_, i)| *i)
        })
        .min()
        .unwrap_or(0);
    let mut out = Vec::new();
    for maximum in [768, 256, 64] {
        if text.len() <= maximum {
            continue;
        }
        let mut start = focus.saturating_sub(maximum / 4);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + maximum).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut coordinate = source.clone();
        coordinate.start += start;
        coordinate.end = source.start + end;
        out.push(EvidenceVariant {
            text: text[start..end].to_string(),
            source: coordinate,
        });
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceCitation {
    /// Response-scoped ID in compact contexts. Empty in the full legacy render.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub citation_id: String,
    pub unit_id: String,
    pub layer_index: usize,
    pub rendered_start: usize,
    pub rendered_end: usize,
    /// Exact delivered text inside this layer, excluding citation framing/support IDs.
    #[serde(default)]
    pub rendered_text_start: usize,
    #[serde(default)]
    pub rendered_text_end: usize,
    pub source: SourceCoordinate,
    pub role: String,
    pub support: Vec<String>,
    pub shortened: bool,
    pub omitted_ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitDecision {
    pub unit_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceResult {
    #[serde(flatten)]
    pub context: ContextResult,
    pub evidence: Vec<EvidenceCitation>,
    #[serde(skip)]
    pub decisions: Vec<UnitDecision>,
}

/// A synthesis can be published only with a nonempty set of live non-derived inputs.
pub fn support_is_live(states: &[(bool, bool, bool)]) -> bool {
    !states.is_empty()
        && states
            .iter()
            .all(|(present, in_space, primary)| *present && *in_space && *primary)
}

/// Reciprocal-rank fusion is independent of SQL and channel score scales.
pub fn fuse_source_channels(
    channels: &[Vec<String>],
    maximum: usize,
) -> (Vec<String>, Vec<String>) {
    fuse_with_weights(channels, maximum, &vec![1.0; channels.len()])
}

/// Bounded query-term channels differ in specificity. Use observed breadth,
/// rather than corpus-language word lists, to prevent several common terms
/// from overwhelming a narrow exact source channel. This is not corpus IDF.
pub fn fuse_query_source_channels(
    channels: &[Vec<String>],
    maximum: usize,
) -> (Vec<String>, Vec<String>) {
    let weights: Vec<_> = channels
        .iter()
        .map(|c| 1.0 / ((1 + c.len()) as f64).sqrt())
        .collect();
    fuse_with_weights(channels, maximum, &weights)
}

fn fuse_with_weights(
    channels: &[Vec<String>],
    maximum: usize,
    weights: &[f64],
) -> (Vec<String>, Vec<String>) {
    let mut scores = std::collections::BTreeMap::<String, f64>::new();
    for (channel, weight) in channels.iter().zip(weights) {
        let mut seen = std::collections::BTreeSet::new();
        for (rank, id) in channel.iter().enumerate() {
            if seen.insert(id) {
                *scores.entry(id.clone()).or_default() += weight / (61 + rank) as f64;
            }
        }
    }
    let mut rows: Vec<_> = scores.into_iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut ids: Vec<_> = rows.into_iter().map(|(id, _)| id).collect();
    let dropped = ids.split_off(maximum.min(ids.len()));
    (ids, dropped)
}
