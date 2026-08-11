//! Deterministic label-propagation community detection (DESIGN §9.4).
//! Tie-break: lowest label value wins. Fixed iteration cap guarantees termination.
use crate::adjacency::AdjacencyGraph;
use std::collections::BTreeMap;
#[derive(Debug, Clone)]
pub struct CommunityMap{ pub labels: BTreeMap<String, u64> }
pub fn label_propagation(graph: &AdjacencyGraph, max_iterations: usize) -> CommunityMap {
    let nodes: Vec<String> = graph.all_nodes();
    let mut labels: BTreeMap<String, u64> = nodes.iter().enumerate().map(|(i,n)|(n.clone(),i as u64)).collect();
    for _ in 0..max_iterations {
        let mut changed = false;
        for node in &nodes {
            let mut nl: Vec<u64> = Vec::new();
            for (n,_,_) in graph.neighbors_out(node) { if let Some(&l)=labels.get(n){nl.push(l);} }
            for (n,_,_) in graph.neighbors_in(node) { if let Some(&l)=labels.get(n){nl.push(l);} }
            if nl.is_empty() { continue; }
            nl.sort_unstable();
            let new_label = most_frequent(&nl);
            if labels.get(node)!=Some(&new_label) { labels.insert(node.clone(),new_label); changed=true; }
        }
        if !changed { break; }
    }
    CommunityMap{ labels }
}
fn most_frequent(sorted: &[u64]) -> u64 {
    let mut best = sorted[0]; let mut best_c = 1; let mut cur = sorted[0]; let mut cur_c = 1;
    for &l in &sorted[1..] { if l==cur { cur_c+=1; } else { if cur_c>best_c { best_c=cur_c; best=cur; } cur=l; cur_c=1; } }
    if cur_c>best_c { best=cur; }
    best
}
