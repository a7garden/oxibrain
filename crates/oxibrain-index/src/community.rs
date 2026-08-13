//! Deterministic label-propagation community detection (DESIGN §9.4).
//! Tie-break: lowest label value wins. Fixed iteration cap guarantees termination.
use crate::adjacency::AdjacencyGraph;
use std::collections::BTreeMap;
#[derive(Debug, Clone)]
pub struct CommunityMap {
    pub labels: BTreeMap<String, u64>,
}
pub fn label_propagation(graph: &AdjacencyGraph, max_iterations: usize) -> CommunityMap {
    let nodes: Vec<String> = graph.all_nodes();
    let mut labels: BTreeMap<String, u64> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u64))
        .collect();
    for _ in 0..max_iterations {
        let mut changed = false;
        for node in &nodes {
            let mut nl: Vec<u64> = Vec::new();
            for (n, _, _) in graph.neighbors_out(node) {
                if let Some(&l) = labels.get(n) {
                    nl.push(l);
                }
            }
            for (n, _, _) in graph.neighbors_in(node) {
                if let Some(&l) = labels.get(n) {
                    nl.push(l);
                }
            }
            if nl.is_empty() {
                continue;
            }
            nl.sort_unstable();
            let new_label = most_frequent(&nl);
            if labels.get(node) != Some(&new_label) {
                labels.insert(node.clone(), new_label);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    CommunityMap { labels }
}
fn most_frequent(sorted: &[u64]) -> u64 {
    let mut best = sorted[0];
    let mut best_c = 1;
    let mut cur = sorted[0];
    let mut cur_c = 1;
    for &l in &sorted[1..] {
        if l == cur {
            cur_c += 1;
        } else {
            if cur_c > best_c {
                best_c = cur_c;
                best = cur;
            }
            cur = l;
            cur_c = 1;
        }
    }
    if cur_c > best_c {
        best = cur;
    }
    best
}

/// Confidence-weighted label propagation (§9.4, 10.6). Same algorithm as
/// `label_propagation` but neighbor votes are weighted by edge weight (mean
/// belief confidence). An edge with weight 0.9 has 3× the vote of weight 0.3.
pub fn label_propagation_weighted(
    graph: &crate::adjacency::WeightedAdjacencyGraph,
    max_iterations: usize,
) -> CommunityMap {
    let nodes = graph.all_nodes();
    let mut labels: BTreeMap<String, u64> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u64))
        .collect();

    for _ in 0..max_iterations {
        let mut changed = false;
        for node in &nodes {
            // Collect weighted votes: (label, total_weight).
            let mut votes: BTreeMap<u64, f64> = BTreeMap::new();
            for (n, w) in graph.neighbors_out(node) {
                if let Some(&l) = labels.get(n) {
                    *votes.entry(l).or_default() += *w;
                }
            }
            for (n, w) in graph.neighbors_in(node) {
                if let Some(&l) = labels.get(n) {
                    *votes.entry(l).or_default() += *w;
                }
            }
            if votes.is_empty() {
                continue;
            }
            // Pick the label with the highest total weight.
            // Tie-break: lowest label value wins (deterministic).
            let new_label = votes
                .iter()
                .max_by(|a, b| {
                    a.1.partial_cmp(b.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.0.cmp(a.0))
                })
                .map(|(l, _)| *l)
                .unwrap_or(0);
            if labels.get(node) != Some(&new_label) {
                labels.insert(node.clone(), new_label);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    CommunityMap { labels }
}
