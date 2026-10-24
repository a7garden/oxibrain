use oxibrain_core::context::{LayerKind, render_context};
use oxibrain_core::evidence::{EvidenceUnit, SourceCoordinate};
use oxibrain_core::evidence_pack::pack_evidence;
use oxibrain_ports::TokenizerPort;
use proptest::prelude::*;
struct Framed;
impl TokenizerPort for Framed {
    // Deliberately nonadditive: separators and framing must be counted globally.
    fn count(&self, s: &str) -> usize {
        if s.is_empty() { 0 } else { s.len() + 7 }
    }
    fn truncate_to(&self, _: &str, _: usize) -> String {
        panic!("packer must not clip a structured assertion")
    }
    fn id(&self) -> &str {
        "nonadditive-test"
    }
}
fn unit(n: usize, text: String, layer: LayerKind) -> EvidenceUnit {
    let len = text.len();
    EvidenceUnit::new(
        layer,
        text,
        SourceCoordinate {
            reference: format!("episode://{n}"),
            revision: "r1".into(),
            view: "episode-utf8/1".into(),
            view_digest: None,
            decoder: None,
            start: 0,
            end: len,
            document_uuid: None,
        },
        "untrusted_observation",
    )
}

fn source_coordinate(start: usize, end: usize) -> SourceCoordinate {
    SourceCoordinate {
        reference: "episode://overlap".into(),
        revision: "r1".into(),
        view: "episode-utf8/1".into(),
        view_digest: None,
        decoder: None,
        start,
        end,
        document_uuid: None,
    }
}

fn source_window(full_text: &str, source: SourceCoordinate, role: &str) -> EvidenceUnit {
    EvidenceUnit::new(
        LayerKind::RecentEpisodes,
        full_text
            .get(source.start..source.end)
            .expect("window must be a UTF-8 range")
            .to_string(),
        source,
        role,
    )
}
#[test]
fn rendered_support_and_utf8_citation_bytes_remain_stable() {
    let mut u = unit(1, "근거".into(), LayerKind::RecentEpisodes);
    u.support = vec!["source-a".into(), "출처-b".into()];
    let out = pack_evidence(&[u], 1000, &Framed);
    let expected = "[episode://1; role=untrusted_observation; view=episode-utf8/1; bytes=0..6; window=0..6; uuid=; excerpt=false] 근거\n  sources: source-a, 출처-b";
    assert_eq!(out.context.layers[0].text, expected);
    assert_eq!(out.evidence[0].rendered_start, 0);
    assert_eq!(out.evidence[0].rendered_end, expected.len());
    assert_eq!(
        out.context.total_tokens,
        Framed.count(&render_context(&out.context))
    );
}

#[test]
fn oversized_first_continues_to_small_second_without_overflow_floor() {
    let units = vec![
        unit(0, "x".repeat(2000), LayerKind::RecentEpisodes),
        unit(1, "needle".into(), LayerKind::RecentEpisodes),
    ];
    let out = pack_evidence(&units, 250, &Framed);
    assert!(render_context(&out.context).contains("needle"));
    assert!(out.context.truncated);
    assert_eq!(out.decisions.len(), 2);
    assert_eq!(out.decisions[0].reason, "budget_excluded");
    let none = pack_evidence(&units[..1], 250, &Framed);
    assert!(none.context.layers.is_empty());
}
#[test]
fn sourceless_summary_is_not_published() {
    let mut u = unit(1, "claimed synthesis".into(), LayerKind::Summaries);
    u.role = "summary".into();
    let r = pack_evidence(&[u], 1000, &Framed);
    assert!(r.context.layers.is_empty());
    assert_eq!(r.decisions[0].reason, "missing_support");
}

#[test]
fn partial_source_overlap_keeps_unique_tail_and_its_condition() {
    let marker = "required condition at byte 3000";
    let mut bytes = vec![b'x'; 3840];
    bytes[3000..3000 + marker.len()].copy_from_slice(marker.as_bytes());
    let full_text = String::from_utf8(bytes).expect("ASCII source fixture");
    let units = vec![
        source_window(
            &full_text,
            source_coordinate(0, 2048),
            "untrusted_observation",
        ),
        source_window(
            &full_text,
            source_coordinate(1792, 3840),
            "untrusted_observation",
        ),
    ];

    let result = pack_evidence(&units, 6000, &Framed);

    assert_eq!(result.evidence.len(), 2);
    assert_eq!(
        result
            .decisions
            .iter()
            .map(|decision| decision.reason.as_str())
            .collect::<Vec<_>>(),
        vec!["selected", "selected"]
    );
    let tail = &result.evidence[1];
    assert_eq!((tail.source.start, tail.source.end), (1792, 3840));
    let rendered = &result.context.layers[tail.layer_index].text;
    let tail_text = &rendered[tail.rendered_text_start..tail.rendered_text_end];
    assert_eq!(tail_text, &full_text[1792..3840]);
    assert!(tail_text.contains(marker));
}

#[test]
fn nested_and_full_duplicate_source_ranges_remain_deduplicated() {
    let full_text = "x".repeat(3000);
    let units = vec![
        source_window(
            &full_text,
            source_coordinate(0, 3000),
            "untrusted_observation",
        ),
        source_window(
            &full_text,
            source_coordinate(500, 1000),
            "untrusted_observation",
        ),
        // A different role gives the equal interval a distinct unit ID, so this
        // verifies source-range suppression rather than ID suppression.
        source_window(&full_text, source_coordinate(0, 3000), "quoted_excerpt"),
    ];

    let result = pack_evidence(&units, 8000, &Framed);

    assert_eq!(result.evidence.len(), 1);
    assert_eq!(result.decisions[0].reason, "selected");
    assert_eq!(result.decisions[1].reason, "duplicate_source_range");
    assert_eq!(result.decisions[2].reason, "duplicate_source_range");
}

#[test]
fn source_superset_is_retained_after_a_nested_range() {
    let full_text = "x".repeat(1500);
    let units = vec![
        source_window(
            &full_text,
            source_coordinate(500, 1000),
            "untrusted_observation",
        ),
        source_window(
            &full_text,
            source_coordinate(0, 1500),
            "untrusted_observation",
        ),
    ];

    let result = pack_evidence(&units, 5000, &Framed);

    assert_eq!(result.evidence.len(), 2);
    assert_eq!(result.decisions[0].reason, "selected");
    assert_eq!(result.decisions[1].reason, "selected");
    assert_eq!(
        (
            result.evidence[1].source.start,
            result.evidence[1].source.end
        ),
        (0, 1500)
    );
}

#[test]
fn distinct_source_revisions_views_and_digests_are_not_deduplicated() {
    let full_text = "x".repeat(1000);
    let base = source_coordinate(0, 1000);
    let mut distinct = Vec::new();

    let mut changed = base.clone();
    changed.revision = "r2".into();
    distinct.push(changed);
    let mut changed = base.clone();
    changed.view = "episode-utf8/2".into();
    distinct.push(changed);
    let mut changed = base.clone();
    changed.view_digest = Some("digest-v2".into());
    distinct.push(changed);
    let mut changed = base.clone();
    changed.decoder = Some("decoder-v2".into());
    distinct.push(changed);
    let mut changed = base;
    changed.document_uuid = Some("document-v2".into());
    distinct.push(changed);

    for other_source in distinct {
        let units = vec![
            source_window(
                &full_text,
                source_coordinate(0, 1000),
                "untrusted_observation",
            ),
            source_window(&full_text, other_source, "untrusted_observation"),
        ];
        let result = pack_evidence(&units, 5000, &Framed);
        assert_eq!(result.evidence.len(), 2);
        assert_eq!(result.decisions[1].reason, "selected");
    }
}

proptest! {
    #[test]
    fn budget_conservation_coordinates_and_determinism(texts in prop::collection::vec(".{0,120}",0..20),budget in 0usize..4000) {
        let units:Vec<_>=texts.into_iter().enumerate().map(|(i,s)|unit(i,s,if i%2==0{LayerKind::Documents}else{LayerKind::RecentEpisodes})).collect();
        let a=pack_evidence(&units,budget,&Framed);let b=pack_evidence(&units,budget,&Framed);
        prop_assert_eq!(a.decisions.len(),units.len());prop_assert!(a.context.total_tokens<=budget);
        prop_assert_eq!(a.context.total_tokens,Framed.count(&render_context(&a.context)));
        prop_assert_eq!(serde_json::to_string(&a).unwrap(),serde_json::to_string(&b).unwrap());
        for c in a.evidence {let s=&a.context.layers[c.layer_index].text;prop_assert!(s.is_char_boundary(c.rendered_start)&&s.is_char_boundary(c.rendered_end));prop_assert!(c.rendered_start<=c.rendered_end&&c.rendered_end<=s.len());}
    }
    #[test]
    fn windows_cover_every_utf8_byte(s in ".{0,5000}",size in 1usize..256,overlap in 0usize..80) {
        let w=oxibrain_core::chunking::source_windows(&s,size,overlap);
        let mut end=0;for c in w {prop_assert!(c.span_start<=end);prop_assert!(c.span_end>c.span_start);prop_assert!(s.is_char_boundary(c.span_start)&&s.is_char_boundary(c.span_end));end=c.span_end;}
        prop_assert_eq!(end,s.len());
    }
}

#[test]
fn common_query_channels_do_not_overwhelm_a_narrow_source_channel() {
    let broad: Vec<_> = (0..64).map(|i| format!("common-{i}")).collect();
    let narrow = vec!["required-primary".into(), "required-environment".into()];
    let channels = vec![
        narrow.clone(),
        narrow.clone(),
        broad.clone(),
        broad.clone(),
        broad.clone(),
        broad,
    ];
    let (kept, dropped) = oxibrain_core::evidence::fuse_query_source_channels(&channels, 2);
    assert_eq!(kept, narrow);
    assert_eq!(dropped.len(), 64);
}

proptest! {
    #[test]
    fn quotation_variants_keep_exact_unicode_source_coordinates(text in ".{0,4000}",query in ".{0,30}") {
        let u=unit(1,text.clone(),LayerKind::RecentEpisodes);
        for v in oxibrain_core::evidence::quotation_variants(&text,&u.source,&query) {
            prop_assert!(v.source.end<=text.len());prop_assert!(text.is_char_boundary(v.source.start)&&text.is_char_boundary(v.source.end));
            prop_assert_eq!(v.text,&text[v.source.start..v.source.end]);
        }
    }
}

#[test]
fn many_structured_claims_cannot_starve_a_fitting_source_procedure() {
    let mut units: Vec<_> = (0..30)
        .map(|i| {
            unit(
                i,
                "structured claim ".repeat(8),
                LayerKind::HighSalienceBeliefs,
            )
        })
        .collect();
    units.push(unit(
        99,
        "procedure: restore the warm cache first".into(),
        LayerKind::RecentEpisodes,
    ));
    let result = pack_evidence(&units, 1200, &Framed);
    assert!(render_context(&result.context).contains("restore the warm cache first"));
    assert_eq!(result.decisions.len(), units.len());
}

#[test]
fn accepted_canonical_counts_are_reused_and_layer_token_counts_are_not_added() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counted {
        calls: AtomicUsize,
    }
    impl oxibrain_ports::TokenizerPort for Counted {
        fn id(&self) -> &str {
            "nonadditive-counted"
        }
        fn count(&self, s: &str) -> usize {
            self.calls.fetch_add(1, Ordering::Relaxed);
            s.as_bytes().windows(2).filter(|w| *w == b"\n\n").count() + s.len() / 3
        }
        fn truncate_to(&self, s: &str, n: usize) -> String {
            s.chars().take(n).collect()
        }
    }
    let tokenizer = Counted {
        calls: AtomicUsize::new(0),
    };
    let units: Vec<_> = (0..64)
        .map(|i| {
            oxibrain_core::evidence::EvidenceUnit::new(
                oxibrain_core::context::LayerKind::Documents,
                format!("payload {i}"),
                oxibrain_core::evidence::SourceCoordinate {
                    reference: format!("doc://{i}"),
                    revision: "r1".into(),
                    view: "decoded-utf8/1".into(),
                    view_digest: None,
                    decoder: None,
                    start: 0,
                    end: 10,
                    document_uuid: None,
                },
                "untrusted_observation",
            )
        })
        .collect();
    let result = oxibrain_core::evidence_pack::pack_evidence(&units, 2000, &tokenizer);
    assert_eq!(tokenizer.calls.load(Ordering::Relaxed), 64);
    assert_eq!(
        result.context.total_tokens,
        tokenizer.count(&oxibrain_core::context::render_context(&result.context))
    );
}

#[test]
fn compact_context_keeps_a_complete_response_scoped_source_and_uncertainty_map() {
    let mut a = unit(
        1,
        "Proceed only after the failed health check.\nuncertainty: conflicting sources".into(),
        LayerKind::Documents,
    );
    a.support = vec!["support-a".into(), "support-b".into()];
    let b = unit(
        2,
        "동등한 권한의 반대 지시".into(),
        LayerKind::RecentEpisodes,
    );
    let policy = oxibrain_core::read_scope::ReadPolicy {
        compact_evidence: true,
        ..Default::default()
    };
    let full = pack_evidence(&[a.clone(), b.clone()], 3000, &Framed);
    let compact =
        oxibrain_core::evidence_pack::pack_evidence_with_policy(&[a, b], 3000, &Framed, &policy);
    let text = render_context(&compact.context);
    assert!(
        text.contains("only after")
            && text.contains("conflicting sources")
            && text.contains("반대 지시")
    );
    assert!(compact.context.total_tokens < full.context.total_tokens);
    assert_eq!(compact.context.total_tokens, Framed.count(&text));
    assert_eq!(compact.evidence.len(), 2);
    for (index, citation) in compact.evidence.iter().enumerate() {
        assert_eq!(citation.citation_id, format!("E{}", index + 1));
        let layer = &compact.context.layers[citation.layer_index].text;
        assert!(
            layer[citation.rendered_start..citation.rendered_end]
                .starts_with(&format!("[{};", citation.citation_id))
        );
        let original = full
            .evidence
            .iter()
            .find(|c| c.unit_id == citation.unit_id)
            .unwrap();
        assert_eq!(citation.source, original.source);
        assert_eq!(citation.support, original.support);
        assert_eq!(citation.omitted_ranges, original.omitted_ranges);
    }
}

proptest! {
    #[test]
    fn compact_binding_and_whole_context_budget_remain_exact(texts in prop::collection::vec(".{1,100}",0..16),budget in 0usize..2000) {
        let units:Vec<_>=texts.iter().enumerate().map(|(i,s)|unit(i,s.clone(),if i%2==0{LayerKind::Documents}else{LayerKind::RecentEpisodes})).collect();
        let policy=oxibrain_core::read_scope::ReadPolicy {compact_evidence:true,..Default::default()};
        let out=oxibrain_core::evidence_pack::pack_evidence_with_policy(&units,budget,&Framed,&policy);
        prop_assert!(out.context.total_tokens<=budget);
        prop_assert_eq!(out.context.total_tokens,Framed.count(&render_context(&out.context)));
        prop_assert_eq!(out.decisions.len(),units.len());
        let packet=oxibrain_core::evidence_reader::from_result(&out).unwrap();
        for (i,e) in packet.iter().enumerate() {
            prop_assert_eq!(&e.id,&format!("E{}",i+1));
            let source=e.source.as_ref().unwrap();
            let ordinal:usize=source.reference.strip_prefix("episode://").unwrap().parse().unwrap();
            prop_assert_eq!(&e.text,&texts[ordinal]);
        }
    }
}
