//! Reciprocal Rank Fusion (Cormack et al. 2009). Pure, deterministic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusedItem {
    pub key: String,
    pub score: f64,
}

pub fn fuse(lists: &[Vec<(String, f64)>], k: u32) -> Vec<FusedItem> {
    let mut scores: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for list in lists {
        for (rank, (key, _raw)) in list.iter().enumerate() {
            *scores.entry(key.as_str()).or_default() += 1.0 / (k as f64 + rank as f64 + 1.0);
        }
    }
    let mut items: Vec<FusedItem> = scores
        .into_iter()
        .map(|(key, score)| FusedItem {
            key: key.to_string(),
            score,
        })
        .collect();
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });
    items
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic() {
        assert_eq!(fuse(&[vec![("a".into(), 1.0)]], 60)[0].key, "a");
    }
}
