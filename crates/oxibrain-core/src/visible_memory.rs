//! Pure historical truth reconstruction; no cached current beliefs are read.
use crate::confidence::CalibrationTable;
use crate::fold::{StatementEntry, fold};
use crate::read_scope::ReadScope;
use crate::registry::PredicateDef;
use crate::{Belief, Statement};
use oxibrain_ports::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct VisibleClaim {
    pub profile_relevant: bool,
    pub assertions: Vec<String>,
    pub statement: Statement,
    pub belief: Belief,
    pub sources: Vec<String>,
}

pub fn visible_claims(
    groups: &[Vec<StatementEntry>],
    defs: &BTreeMap<String, PredicateDef>,
    live: &BTreeSet<String>,
    scope: &ReadScope,
    now: Timestamp,
) -> Vec<VisibleClaim> {
    let mut out = Vec::new();
    let known = scope.known_at.unwrap_or(now);
    let valid = scope.as_of.unwrap_or(now);
    for group in groups {
        let Some(first) = group.first() else {
            continue;
        };
        let Some(def) = defs.get(&first.statement.predicate) else {
            continue;
        };
        let mut input = group.clone();
        for e in &mut input {
            e.assertions.retain(|a| live.contains(&a.episode));
        }
        for b in fold(def, &input, known, &CalibrationTable::default()) {
            if b.valid_from > valid || b.valid_to < valid || b.confidence < scope.min_confidence {
                continue;
            }
            let entry = input
                .iter()
                .find(|e| e.statement.id == b.statement)
                .expect("fold returns input statement");
            let mut sources: Vec<_> = entry
                .assertions
                .iter()
                .filter(|a| a.recorded_at <= known && a.retracted_at.is_none_or(|t| t > known))
                .map(|a| a.episode.clone())
                .collect();
            sources.sort();
            sources.dedup();
            if sources.is_empty() {
                continue;
            }
            let assertions = entry
                .assertions
                .iter()
                .filter(|a| a.recorded_at <= known && a.retracted_at.is_none_or(|t| t > known))
                .map(|a| a.id.clone())
                .collect();
            out.push(VisibleClaim {
                assertions,
                profile_relevant: def.profile_relevant,
                statement: entry.statement.clone(),
                belief: b,
                sources,
            });
        }
    }
    out.sort_by(|a, b| a.statement.id.cmp(&b.statement.id));
    out
}

#[derive(Debug, Clone)]
pub struct ClaimHit {
    pub subject: String,
    pub statement: String,
    pub text: String,
    pub score: f64,
    pub sources: Vec<String>,
}

pub fn search_claims(
    claims: &[VisibleClaim],
    names: &BTreeMap<String, String>,
    query: &str,
    limit: usize,
) -> Vec<ClaimHit> {
    let query = query.to_lowercase();
    let shingles = oxibrain_index::ngram::shingles(&query, 3);
    let mut out = Vec::new();
    for c in claims {
        if c.sources.is_empty() {
            continue;
        }
        let text = render_claim(c, names);
        let subject = names
            .get(&c.statement.subject)
            .unwrap_or(&c.statement.subject);
        let object = match &c.statement.object {
            crate::Object::Entity(id) => names.get(id).cloned().unwrap_or(id.clone()),
            crate::Object::Literal(v) => serde_json::to_string(v).expect("literal serializes"),
        };
        let normalized = format!("{subject} {} {object}", c.statement.predicate).to_lowercase();
        let score = oxibrain_index::ngram::jaccard(
            &shingles,
            &oxibrain_index::ngram::shingles(&normalized, 3),
        );
        if score > 0.0 && !query.is_empty() {
            out.push(ClaimHit {
                subject: c.statement.subject.clone(),
                statement: c.statement.id.clone(),
                text,
                score,
                sources: c.sources.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.statement.cmp(&b.statement))
    });
    out.truncate(limit);
    out
}

pub fn render_claim(c: &VisibleClaim, names: &BTreeMap<String, String>) -> String {
    let subject = names
        .get(&c.statement.subject)
        .unwrap_or(&c.statement.subject);
    let object = match &c.statement.object {
        crate::Object::Entity(id) => names.get(id).cloned().unwrap_or(id.clone()),
        crate::Object::Literal(v) => serde_json::to_string(v).expect("literal serializes"),
    };
    let text = format!(
        "{} {} {} (valid={}..{}; status={:?}; confidence={:.2}; affirm={}; deny={}; assertions={})",
        subject,
        c.statement.predicate,
        object,
        c.belief.valid_from.0,
        c.belief.valid_to.0,
        c.belief.status,
        c.belief.confidence,
        c.belief.support.affirm_count,
        c.belief.support.deny_count,
        c.assertions.join(",")
    );
    text
}

/// One-hop expansion uses only the same scoped claims as lexical rendering.
/// No current adjacency or unscoped surface lookup may enrich this graph.
pub fn expand_claim_hits(
    claims: &[VisibleClaim],
    names: &BTreeMap<String, String>,
    hits: &[ClaimHit],
    maximum: usize,
) -> Vec<ClaimHit> {
    let seeds: BTreeSet<_> = hits.iter().take(4).map(|h| h.subject.clone()).collect();
    let mut neighbors = seeds.clone();
    for c in claims {
        if let crate::Object::Entity(id) = &c.statement.object {
            if seeds.contains(&c.statement.subject) {
                neighbors.insert(id.clone());
            }
            if seeds.contains(id) {
                neighbors.insert(c.statement.subject.clone());
            }
        }
    }
    let seen: BTreeSet<_> = hits.iter().map(|h| h.statement.as_str()).collect();
    claims
        .iter()
        .filter(|c| {
            neighbors.contains(&c.statement.subject)
                && !seen.contains(c.statement.id.as_str())
                && !c.sources.is_empty()
        })
        .take(maximum)
        .map(|c| ClaimHit {
            subject: c.statement.subject.clone(),
            statement: c.statement.id.clone(),
            text: render_claim(c, names),
            score: 0.0,
            sources: c.sources.clone(),
        })
        .collect()
}

/// Fuse scored dense claims with lexical claims, then expand the scoped graph.
/// Adapter output is validated by the facade; scoring here is pure.
pub fn combine_claim_hits(
    claims: &[VisibleClaim],
    names: &BTreeMap<String, String>,
    lexical: Vec<ClaimHit>,
    dense: &[(String, f64)],
    graph: bool,
    maximum: usize,
) -> Vec<ClaimHit> {
    let mut dense = dense.to_vec();
    dense.retain(|(_, s)| s.is_finite());
    dense.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let channels = [
        lexical.iter().map(|h| h.statement.clone()).collect(),
        dense.iter().map(|(id, _)| id.clone()).collect(),
    ];
    let (ids, _) = crate::evidence::fuse_source_channels(&channels, maximum);
    let mut out: Vec<_> = ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            claims
                .iter()
                .find(|c| &c.statement.id == id)
                .map(|c| ClaimHit {
                    subject: c.statement.subject.clone(),
                    statement: id.clone(),
                    text: render_claim(c, names),
                    score: 1.0 / (i + 1) as f64,
                    sources: c.sources.clone(),
                })
        })
        .collect();
    if graph {
        out.extend(expand_claim_hits(
            claims,
            names,
            &out,
            maximum.saturating_sub(out.len()),
        ));
    }
    out
}

pub fn cosine_scores(ids: &[String], vectors: &[Vec<f32>]) -> Vec<(String, f64)> {
    let Some(query) = vectors.first() else {
        return Vec::new();
    };
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    ids.iter()
        .zip(vectors.iter().skip(1))
        .map(|(id, v)| {
            let denominator = norm(query) * norm(v);
            let score = if denominator > 0.0 {
                query
                    .iter()
                    .zip(v)
                    .map(|(a, b)| f64::from(*a) * f64::from(*b))
                    .sum::<f64>()
                    / denominator
            } else {
                0.0
            };
            (id.clone(), score)
        })
        .collect()
}

/// A computed belief's revision includes its complete scoped support, rather
/// than using a constant fold label as if it identified an immutable source.
pub fn claim_revision(claim: &VisibleClaim, names: &BTreeMap<String, String>) -> String {
    let bytes = serde_json::to_vec(&(
        &claim.statement.id,
        &claim.belief,
        &claim.assertions,
        &claim.sources,
        render_claim(claim, names),
    ))
    .expect("claim serializes");
    format!("blake3:{}", blake3::hash(&bytes).to_hex())
}

/// Resolve display identities through only merge declarations visible at K.
/// Statements retain their immutable IDs and original mention provenance.
pub fn scoped_names(
    mut names: BTreeMap<String, String>,
    merges: &[crate::EntityMerge],
    merge_surfaces: &BTreeMap<String, String>,
    live: &BTreeSet<String>,
    known: Timestamp,
) -> BTreeMap<String, String> {
    let mut visible: Vec<_> = merges
        .iter()
        .filter(|m| {
            m.decided_at <= known
                && m.undone_at.is_none_or(|t| t > known)
                && live.contains(&m.provenance)
        })
        .collect();
    visible.sort_by(|a, b| a.decided_at.cmp(&b.decided_at).then(a.id.cmp(&b.id)));
    let mut redirects = BTreeMap::new();
    for m in visible {
        if let Some(surface) = merge_surfaces.get(&m.id) {
            names
                .entry(m.winner.clone())
                .or_insert_with(|| surface.clone());
        }
        redirects.insert(m.loser.clone(), m.winner.clone());
    }
    let original = names.clone();
    for id in original.keys() {
        let mut target = id;
        let mut seen = BTreeSet::new();
        while let Some(next) = redirects.get(target) {
            if !seen.insert(target) {
                break;
            }
            target = next;
        }
        if let Some(name) = original.get(target) {
            names.insert(id.clone(), name.clone());
        }
    }
    names
}

/// Bounded streaming top-k; scores never become fold inputs.
pub fn retain_dense_candidates(rows: &mut Vec<(String, f64)>, maximum: usize) {
    rows.retain(|(_, s)| s.is_finite());
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.truncate(maximum);
}
