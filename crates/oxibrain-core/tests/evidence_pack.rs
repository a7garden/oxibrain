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
