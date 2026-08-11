//! Adjacency graph view over statements (subject→object edges). Pure data structure for BFS traversal.
use oxibrain_core::retrieval::{Direction, PredicateFilter};
use std::collections::{BTreeMap, BTreeSet};
pub struct AdjacencyGraph {
    nodes: BTreeSet<String>,
    outgoing: BTreeMap<String, Vec<(String, String, String)>>,
    incoming: BTreeMap<String, Vec<(String, String, String)>>,
}
impl AdjacencyGraph {
    pub fn new() -> Self {
        Self {
            nodes: BTreeSet::new(),
            outgoing: BTreeMap::new(),
            incoming: BTreeMap::new(),
        }
    }
    pub fn add_edge(&mut self, from: &str, to: &str, pred: &str, stmt: &str) {
        self.nodes.insert(from.into());
        self.nodes.insert(to.into());
        self.outgoing
            .entry(from.into())
            .or_default()
            .push((to.into(), pred.into(), stmt.into()));
        self.incoming
            .entry(to.into())
            .or_default()
            .push((from.into(), pred.into(), stmt.into()));
    }
    pub fn neighbors_out(&self, e: &str) -> &[(String, String, String)] {
        self.outgoing.get(e).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn neighbors_in(&self, e: &str) -> &[(String, String, String)] {
        self.incoming.get(e).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
    pub fn all_nodes(&self) -> Vec<String> {
        self.nodes.iter().cloned().collect()
    }
    pub fn bfs(&self, spec: &BfsSpec) -> BfsResult {
        let mut visited: BTreeMap<String, u8> = BTreeMap::new();
        let mut edges: Vec<(String, String, String, String, u8)> = Vec::new();
        let mut truncated = false;
        let mut frontier: BTreeSet<String> = spec.start.iter().cloned().collect();
        for s in &frontier {
            visited.insert(s.clone(), 0);
        }
        for depth in 1..=spec.max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next: BTreeSet<String> = BTreeSet::new();
            for e in &frontier {
                if visited.len() as u32 >= spec.max_nodes {
                    truncated = true;
                    break;
                }
                let nbrs: Vec<(String, String, String)> = match spec.direction {
                    Direction::Out => self.neighbors_out(e).to_vec(),
                    Direction::In => self.neighbors_in(e).to_vec(),
                    Direction::Both => {
                        let mut c: Vec<(String, String, String)> = Vec::new();
                        c.extend(self.neighbors_out(e).iter().cloned());
                        c.extend(self.neighbors_in(e).iter().cloned());
                        c
                    }
                };
                for (n, p, s) in nbrs {
                    if !spec.predicate_filter.allows(&p) {
                        continue;
                    }
                    edges.push((e.clone(), n.clone(), p.clone(), s.clone(), depth));
                    if !visited.contains_key(&n) {
                        if visited.len() as u32 >= spec.max_nodes {
                            truncated = true;
                            break;
                        }
                        visited.insert(n.clone(), depth);
                        next.insert(n.clone());
                    }
                }
                if truncated {
                    break;
                }
            }
            frontier = next;
            if truncated {
                break;
            }
        }
        BfsResult {
            nodes: visited,
            edges,
            truncated,
        }
    }
}
impl Default for AdjacencyGraph {
    fn default() -> Self {
        Self::new()
    }
}
pub struct BfsSpec {
    pub start: Vec<String>,
    pub max_depth: u8,
    pub max_nodes: u32,
    pub direction: Direction,
    pub predicate_filter: PredicateFilter,
}
pub struct BfsResult {
    pub nodes: BTreeMap<String, u8>,
    pub edges: Vec<(String, String, String, String, u8)>,
    pub truncated: bool,
}
