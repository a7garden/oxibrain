//! Query-dependent selection of complete truth groups. No ranking state enters fold.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone)]
pub struct GroupObservation {
    pub subject: String,
    pub predicate: String,
    pub bytes: usize,
    pub search_text: String,
    pub seed_truncated: bool,
}
#[derive(Debug, Clone)]
pub struct ScoredGroup {
    pub group: GroupObservation,
    pub score: f64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TruthSelectionReceipt {
    pub examined: usize,
    pub materialized: usize,
    pub omitted: usize,
    pub bytes: usize,
    #[serde(default)]
    pub sources: usize,
    pub truncated_seeds: usize,
    pub selection_ms: f64,
    pub fold_ms: f64,
}

/// Keep a bounded seed set while examining metadata in pages, before materializing
/// any fold input. Exact terms outrank partial n-gram matches; ties use identity.
pub fn consider_groups(
    pool: &mut Vec<ScoredGroup>,
    rows: Vec<GroupObservation>,
    query: &str,
    maximum: usize,
) {
    let query = crate::resolution::normalize(query, &String::new());
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let shingles = oxibrain_index::ngram::shingles(&query, 3);
    for group in rows {
        let text = crate::resolution::normalize(&group.search_text, &String::new());
        let exact = terms.iter().filter(|term| text.contains(**term)).count() as f64;
        let partial =
            oxibrain_index::ngram::jaccard(&shingles, &oxibrain_index::ngram::shingles(&text, 3));
        pool.push(ScoredGroup {
            group,
            score: exact + partial,
        });
    }
    pool.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.group.subject.cmp(&b.group.subject))
            .then(a.group.predicate.cmp(&b.group.predicate))
    });
    pool.truncate(maximum);
}

pub fn select_complete_groups(
    pool: Vec<ScoredGroup>,
    maximum_bytes: usize,
) -> (Vec<GroupObservation>, usize) {
    let mut selected = Vec::new();
    let mut used = 0usize;
    for row in pool {
        if row.group.bytes <= maximum_bytes.saturating_sub(used) {
            used += row.group.bytes;
            selected.push(row.group);
        }
    }
    (selected, used)
}
