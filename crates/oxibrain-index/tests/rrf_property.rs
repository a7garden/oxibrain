//! Property tests for Reciprocal Rank Fusion (AGENTS.md §14.3).
//!
//! RRF fuses multiple ranked lists into a single ranking using
//! score = Σ 1/(k + rank + 1). The algorithm is pure and deterministic;
//! these tests verify its invariants over randomly generated inputs.

use oxibrain_index::FusedItem;
use oxibrain_index::rrf::fuse;
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

/// Generate a ranked list with keys from a small set (allows duplicates).
fn arb_ranked_list() -> impl Strategy<Value = Vec<(String, f64)>> {
    prop::collection::vec(
        prop_oneof![
            Just("a".to_string()),
            Just("b".to_string()),
            Just("c".to_string()),
            Just("d".to_string()),
            Just("e".to_string()),
        ],
        0..6,
    )
    .prop_map(|keys| keys.into_iter().map(|k| (k, 1.0)).collect())
}

proptest! {
    /// Output is always sorted by (score desc, key asc).
    #[test]
    fn output_sorted_by_score_then_key(
        lists in prop::collection::vec(arb_ranked_list(), 0..4),
        k in 1u32..100,
    ) {
        let result = fuse(&lists, k);
        for w in result.windows(2) {
            // w[0] should come before w[1] in sort order.
            let ord = w[1]
                .score
                .partial_cmp(&w[0].score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| w[0].key.cmp(&w[1].key));
            prop_assert!(
                ord != std::cmp::Ordering::Greater,
                "output must be sorted by score desc, key asc"
            );
        }
    }

    /// Swapping the order of input lists doesn't change the output scores.
    /// (Score summation is commutative.)
    #[test]
    fn list_order_invariant(
        list_a in arb_ranked_list(),
        list_b in arb_ranked_list(),
        k in 1u32..100,
    ) {
        let result_ab = fuse(&[list_a.clone(), list_b.clone()], k);
        let result_ba = fuse(&[list_b, list_a], k);

        let scores_ab: HashMap<&str, f64> =
            result_ab.iter().map(|i| (i.key.as_str(), i.score)).collect();
        let scores_ba: HashMap<&str, f64> =
            result_ba.iter().map(|i| (i.key.as_str(), i.score)).collect();

        prop_assert_eq!(scores_ab.len(), scores_ba.len(), "same key count");
        for (key, score) in &scores_ab {
            let other = scores_ba.get(*key).copied().unwrap_or(0.0);
            prop_assert!(
                (score - other).abs() < 1e-9,
                "key {key}: score mismatch {score} vs {other}"
            );
        }
    }

    /// All keys from all input lists appear in the output.
    #[test]
    fn all_keys_present(
        lists in prop::collection::vec(arb_ranked_list(), 1..4),
        k in 1u32..100,
    ) {
        let input_keys: HashSet<&str> =
            lists.iter().flat_map(|l| l.iter().map(|(k, _)| k.as_str())).collect();
        let result = fuse(&lists, k);
        let output_keys: HashSet<&str> =
            result.iter().map(|i| i.key.as_str()).collect();
        prop_assert_eq!(input_keys, output_keys, "all input keys must appear in output");
    }

    /// Empty input → empty output.
    #[test]
    fn empty_input_empty_output(k in 1u32..100) {
        let result: Vec<FusedItem> = fuse(&[], k);
        prop_assert!(result.is_empty());
    }

    /// Single list with unique keys: scores are strictly decreasing by rank.
    #[test]
    fn single_list_rank_monotone(n in 2usize..6, k in 1u32..100) {
        let list: Vec<(String, f64)> = (0..n)
            .map(|i| ((b'a' + i as u8) as char).to_string())
            .map(|c| (c, 1.0))
            .collect();
        let result = fuse(&[list], k);
        // With unique keys, each rank gets a strictly lower score.
        for w in result.windows(2) {
            prop_assert!(
                w[0].score > w[1].score + 1e-9,
                "rank-earlier item must have strictly higher score"
            );
        }
    }

    /// A key appearing in more lists accumulates a higher score.
    #[test]
    fn cross_list_accumulation(
        key in "[a-e]",
        k in 1u32..100,
    ) {
        // Key appears in 2 lists vs 1 list.
        let with_dup = fuse(
            &[vec![(key.clone(), 1.0), ("z".into(), 1.0)], vec![(key.clone(), 1.0)]],
            k,
        );
        let without_dup = fuse(
            &[vec![(key.clone(), 1.0), ("z".into(), 1.0)], vec![("z".into(), 1.0)]],
            k,
        );

        let score_dup = with_dup.iter().find(|i| i.key == key).map(|i| i.score).unwrap();
        let score_nodup = without_dup.iter().find(|i| i.key == key).map(|i| i.score).unwrap();
        prop_assert!(
            score_dup > score_nodup,
            "key in 2 lists ({score_dup}) must outrank key in 1 list ({score_nodup})"
        );
    }
}
