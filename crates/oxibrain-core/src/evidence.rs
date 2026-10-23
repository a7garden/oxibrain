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
    pub unit_id: String,
    pub layer_index: usize,
    pub rendered_start: usize,
    pub rendered_end: usize,
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
