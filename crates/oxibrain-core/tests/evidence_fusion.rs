use oxibrain_core::evidence::{fuse_query_source_channels, fuse_query_source_families};
use proptest::prelude::*;
use std::collections::BTreeSet;

#[test]
fn lexical_only_keeps_the_existing_weighted_order() {
    let channels = vec![
        vec!["shared".into(), "second".into(), "third".into()],
        vec!["first".into(), "shared".into()],
        vec!["first".into(), "other".into()],
    ];

    assert_eq!(
        fuse_query_source_families(&channels, &[], 4),
        fuse_query_source_channels(&channels, 4)
    );
}

#[test]
fn dense_only_preserves_rank_order_while_deduplicating() {
    let dense = vec![
        "z-first".into(),
        "a-second".into(),
        "z-first".into(),
        "m-third".into(),
    ];

    assert_eq!(
        fuse_query_source_families(&[], &dense, 2),
        (
            vec!["z-first".into(), "a-second".into()],
            vec!["m-third".into()]
        )
    );
}

#[test]
fn lexical_term_count_growth_cannot_outvote_the_dense_family() {
    let make_lexical = |count| {
        (0..count)
            .map(|_| vec!["z-common".into(), "shared".into()])
            .collect::<Vec<_>>()
    };
    let dense = vec!["a-dense".into(), "shared".into()];

    let one_term = fuse_query_source_families(&make_lexical(1), &dense, 2);
    let many_terms = fuse_query_source_families(&make_lexical(32), &dense, 2);
    assert_eq!(one_term, many_terms);
    assert_eq!(many_terms.0, vec!["shared", "a-dense"]);

    // The legacy single-level weighting lets repeated common lexical channels
    // push the dense-only result outside the same final budget.
    let flattened = make_lexical(32)
        .into_iter()
        .chain([dense.clone()])
        .collect::<Vec<_>>();
    assert!(
        !fuse_query_source_channels(&flattened, 2)
            .0
            .iter()
            .any(|id| id == "a-dense")
    );
}

#[test]
fn complementary_families_can_rescue_a_candidate_after_a_long_lexical_tail() {
    let lexical = vec![
        (0..120)
            .map(|i| format!("lexical-{i:03}"))
            .chain(std::iter::once("rescued".into()))
            .collect(),
    ];
    let dense = vec!["rescued".into()];

    assert_eq!(
        fuse_query_source_families(&lexical, &dense, 1).0,
        vec!["rescued"]
    );
}

#[test]
fn a_long_noisy_dense_tail_does_not_change_lexical_first_tie_order() {
    let lexical = vec![vec!["a-lexical".into(), "lexical-tail".into()]];
    let dense = (0..128)
        .map(|i| format!("z-noise-{i:03}"))
        .collect::<Vec<_>>();

    assert_eq!(
        fuse_query_source_families(&lexical, &dense, 1).0,
        vec!["a-lexical"]
    );
}

#[test]
fn selected_and_dropped_conserve_the_unique_candidate_union() {
    let lexical = vec![
        vec!["a".into(), "b".into(), "b".into(), "c".into()],
        vec!["b".into(), "d".into(), "a".into()],
    ];
    let dense = vec!["d".into(), "e".into(), "d".into(), "a".into()];
    let expected: BTreeSet<_> = lexical
        .iter()
        .flatten()
        .chain(dense.iter())
        .cloned()
        .collect();

    let (selected, dropped) = fuse_query_source_families(&lexical, &dense, 3);
    let selected_set: BTreeSet<_> = selected.iter().cloned().collect();
    let dropped_set: BTreeSet<_> = dropped.iter().cloned().collect();
    let combined: BTreeSet<_> = selected.iter().chain(&dropped).cloned().collect();

    assert_eq!(selected.len(), selected_set.len());
    assert_eq!(dropped.len(), dropped_set.len());
    assert!(selected_set.is_disjoint(&dropped_set));
    assert_eq!(combined, expected);
    assert_eq!(selected.len() + dropped.len(), expected.len());
}

proptest! {
    #[test]
    fn randomized_fusion_is_a_deterministic_bounded_partition(
        lexical_ids in prop::collection::vec(
            prop::collection::vec(0u8..8, 0..8),
            0..6,
        ),
        dense_ids in prop::collection::vec(0u8..8, 0..12),
        maximum in 0usize..16,
    ) {
        let lexical: Vec<Vec<String>> = lexical_ids
            .into_iter()
            .map(|channel| channel.into_iter().map(|id| format!("id-{id}")).collect())
            .collect();
        let dense: Vec<String> = dense_ids
            .into_iter()
            .map(|id| format!("id-{id}"))
            .collect();
        let expected: BTreeSet<_> = lexical
            .iter()
            .flatten()
            .chain(dense.iter())
            .cloned()
            .collect();

        let result = fuse_query_source_families(&lexical, &dense, maximum);
        let repeated = fuse_query_source_families(&lexical, &dense, maximum);
        let selected: BTreeSet<_> = result.0.iter().cloned().collect();
        let dropped: BTreeSet<_> = result.1.iter().cloned().collect();
        let combined: BTreeSet<_> = result.0.iter().chain(&result.1).cloned().collect();

        prop_assert_eq!(&result, &repeated);
        prop_assert!(result.0.len() <= maximum);
        prop_assert_eq!(result.0.len(), selected.len());
        prop_assert_eq!(result.1.len(), dropped.len());
        prop_assert!(selected.is_disjoint(&dropped));
        prop_assert_eq!(&combined, &expected);
        prop_assert_eq!(result.0.len() + result.1.len(), expected.len());
    }
}
