use oxibrain_core::evidence::{fuse_query_source_channels, fuse_query_source_families};
use oxibrain_core::query_candidates::{
    LexicalCandidateChannel, LexicalCandidateKind, fuse_lexical_candidates,
    fuse_lexical_dense_families,
};
use proptest::prelude::*;
use std::collections::BTreeSet;

#[test]
fn document_empty_dense_preserves_exact_lexical_scores_and_drops() {
    let lexical = vec![
        LexicalCandidateChannel {
            kind: LexicalCandidateKind::Term,
            ids: vec!["broad".into(), "narrow".into(), "tail".into()],
            breadth: 1000,
        },
        LexicalCandidateChannel {
            kind: LexicalCandidateKind::ExactQuery,
            ids: vec!["narrow".into()],
            breadth: 1,
        },
    ];
    for maximum in 0..5 {
        assert_eq!(
            fuse_lexical_dense_families(&lexical, &[], maximum),
            fuse_lexical_candidates(&lexical, maximum)
        );
    }
}

#[test]
fn document_dense_only_deduplicates_before_scoring_and_capping() {
    let dense = vec!["z".into(), "z".into(), "a".into(), "m".into()];
    assert_eq!(
        fuse_lexical_dense_families(&[], &dense, 2),
        (
            vec![("z".into(), 1.0 / 61.0), ("a".into(), 1.0 / 62.0)],
            vec!["m".into()]
        )
    );
}

#[test]
fn document_family_fusion_caps_after_combining_complementary_candidates() {
    let lexical = vec![LexicalCandidateChannel {
        kind: LexicalCandidateKind::Term,
        ids: (0..120)
            .map(|i| format!("lexical-{i:03}"))
            .chain(["rescued".into()])
            .collect(),
        breadth: 121,
    }];
    assert_eq!(
        fuse_lexical_dense_families(&lexical, &["rescued".into()], 1).0[0].0,
        "rescued"
    );
}

#[test]
fn document_lexical_breadth_and_expansion_cannot_discount_the_dense_family() {
    let make_channels = |breadth, count| {
        (0..count)
            .map(|_| LexicalCandidateChannel {
                kind: LexicalCandidateKind::Term,
                ids: vec!["z-lexical".into(), "tail".into()],
                breadth,
            })
            .collect::<Vec<_>>()
    };
    let dense = vec!["a-semantic".into()];
    let baseline = fuse_lexical_dense_families(&make_channels(2, 1), &dense, 2);
    assert_eq!(
        baseline
            .0
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["z-lexical", "a-semantic"]
    );
    assert_eq!(
        baseline,
        fuse_lexical_dense_families(&make_channels(10_000, 32), &dense, 2)
    );
}

#[test]
fn document_equal_scores_prefer_lexical_rank_independently_of_channel_order() {
    let mut lexical = vec![
        LexicalCandidateChannel {
            kind: LexicalCandidateKind::ExactQuery,
            ids: vec!["z-first".into(), "a-second".into()],
            breadth: 2,
        },
        LexicalCandidateChannel {
            kind: LexicalCandidateKind::Term,
            ids: vec!["noise".into()],
            breadth: 1000,
        },
    ];
    let dense = vec!["a-second".into(), "z-first".into()];
    let result = fuse_lexical_dense_families(&lexical, &dense, 3);
    assert_eq!(result.0[0].1, result.0[1].1);
    assert_eq!(result.0[0].0, "z-first");
    assert_eq!(result.0[1].0, "a-second");
    assert_eq!(
        fuse_lexical_dense_families(&lexical, &dense, 1).0[0].0,
        "z-first"
    );
    lexical.reverse();
    assert_eq!(result, fuse_lexical_dense_families(&lexical, &dense, 3));
    assert_eq!(
        result,
        fuse_lexical_dense_families(
            &lexical,
            &[
                "a-second".into(),
                "a-second".into(),
                "z-first".into(),
                "z-first".into()
            ],
            3,
        )
    );
}

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
    fn document_fusion_is_a_deterministic_scored_partition(
        lexical_ids in prop::collection::vec(prop::collection::vec(0u8..8, 0..8), 0..6),
        dense_ids in prop::collection::vec(0u8..8, 0..12),
        maximum in 0usize..16,
    ) {
        let lexical: Vec<_> = lexical_ids.into_iter().map(|ids| LexicalCandidateChannel {
            kind: LexicalCandidateKind::Term,
            breadth: ids.len(),
            ids: ids.into_iter().map(|id| format!("id-{id}")).collect(),
        }).collect();
        let dense: Vec<String> = dense_ids.into_iter().map(|id| format!("id-{id}")).collect();
        let expected: BTreeSet<_> = lexical.iter().flat_map(|channel| channel.ids.iter()).chain(dense.iter()).cloned().collect();
        let result = fuse_lexical_dense_families(&lexical, &dense, maximum);
        prop_assert_eq!(&result, &fuse_lexical_dense_families(&lexical, &dense, maximum));
        let selected: BTreeSet<_> = result.0.iter().map(|(id, _)| id.clone()).collect();
        let dropped: BTreeSet<_> = result.1.iter().cloned().collect();
        prop_assert!(result.0.len() <= maximum);
        prop_assert_eq!(result.0.len(), selected.len());
        prop_assert_eq!(result.1.len(), dropped.len());
        prop_assert!(selected.is_disjoint(&dropped));
        prop_assert_eq!(selected.union(&dropped).cloned().collect::<BTreeSet<_>>(), expected);
        prop_assert!(result.0.iter().all(|(_, score)| score.is_finite() && *score > 0.0));
        let (lexical_ranking, _) = fuse_lexical_candidates(&lexical, usize::MAX);
        let lexical_rank = |id: &str| lexical_ranking.iter().position(|(candidate, _)| candidate == id).unwrap_or(usize::MAX);
        prop_assert!(result.0.windows(2).all(|pair| pair[0].1 > pair[1].1 || (pair[0].1 == pair[1].1 && (lexical_rank(&pair[0].0), &pair[0].0) < (lexical_rank(&pair[1].0), &pair[1].0))));
    }

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
