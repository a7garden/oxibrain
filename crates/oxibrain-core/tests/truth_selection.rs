use oxibrain_core::truth_selection::{GroupObservation, consider_groups, select_complete_groups};
use proptest::prelude::*;
proptest! {
    #[test]
    fn relevant_group_survives_unrelated_metadata_and_budget_is_conserved(n in 0usize..400,budget in 400usize..3000){
        let target=GroupObservation{subject:"target".into(),predicate:"p".into(),bytes:400,search_text:"UniqueRequestedIdentifier".into(),seed_truncated:false};
        let mut pool=Vec::new();
        for i in 0..n {
            consider_groups(&mut pool,vec![GroupObservation{subject:format!("unrelated-{i}"),predicate:"p".into(),bytes:400,search_text:"Other observation".into(),seed_truncated:false}],"UniqueRequestedIdentifier",32);
        }
        consider_groups(&mut pool,vec![target],"UniqueRequestedIdentifier",32);
        let (selected,bytes)=select_complete_groups(pool,budget);
        prop_assert_eq!(selected[0].subject.as_str(),"target");
        prop_assert!(bytes<=budget);
        prop_assert_eq!(selected.iter().map(|g|g.bytes).sum::<usize>(),bytes);
    }
}

proptest! {
    #[test]
    fn paged_dense_top_k_matches_a_complete_stable_sort(scores in proptest::collection::vec(-100i32..101, 0..300), maximum in 0usize..100, page in 1usize..64) {
        let rows: Vec<_> = scores.into_iter().enumerate().map(|(i, score)| (format!("source-{i:04}"), f64::from(score))).collect();
        let mut expected = rows.clone();
        expected.sort_by(|a,b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        expected.truncate(maximum);
        let mut actual = Vec::new();
        for batch in rows.chunks(page) {
            actual.extend_from_slice(batch);
            oxibrain_core::visible_memory::retain_dense_candidates(&mut actual, maximum);
        }
        prop_assert_eq!(actual, expected);
    }
}
