//! Tests for Task 1: pure document planners and observation/cache types.
//!
//! Covers the `oxibrain_core::documents` module:
//!   - `diff_roots` Keep/Reset/Remove table over RootFingerprint equality
//!   - `plan_reconcile` conservation + determinism (sorted by locator)
//!   - `document_id` / `chunk_id` stability (fixed vectors)
//!   - revision-hint change ⇒ Replace
//!   - proptest conservation over random inputs

use oxibrain_core::documents::{
    CachedFile, CachedRootMeta, FileAction, FileObservation, RootAction, RootFingerprint, chunk_id,
    diff_roots, document_id, plan_reconcile,
};
use proptest::prelude::*;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn obs(
    locator: &str,
    bytes: u64,
    modified_ns: i64,
    revision_hint: Option<&str>,
) -> FileObservation {
    FileObservation {
        locator: locator.into(),
        bytes,
        modified_ns,
        revision_hint: revision_hint.map(str::to_owned),
    }
}

fn cached(locator: &str, bytes: u64, modified_ns: i64, revision: &str) -> CachedFile {
    CachedFile {
        locator: locator.into(),
        bytes,
        modified_ns,
        revision: revision.into(),
    }
}

fn fp(alias: &str, space: &str, path: &str) -> RootFingerprint {
    RootFingerprint {
        alias: alias.into(),
        canonical_path: path.into(),
        space: space.into(),
        include: vec!["**/*.md".into()],
        exclude: vec![],
        max_file_bytes: 1024,
        decoder_version: "1".into(),
    }
}

fn meta(alias: &str, space: &str, path: &str, generation: i64) -> CachedRootMeta {
    CachedRootMeta {
        alias: alias.into(),
        space: space.into(),
        fingerprint: fp(alias, space, path),
        generation,
    }
}

// ── diff_roots table ───────────────────────────────────────────────────────

#[test]
fn diff_roots_keep_when_fingerprint_equal() {
    let configured = vec![fp("vault", "personal", "/data/vault")];
    let cached = vec![meta("vault", "personal", "/data/vault", 3)];
    let actions = diff_roots(&configured, &cached);
    assert_eq!(actions, vec![("vault".into(), RootAction::KeepRoot)]);
}

#[test]
fn diff_roots_remove_when_alias_disappears() {
    let configured = vec![fp("vault", "personal", "/data/vault")];
    let cached = vec![
        meta("vault", "personal", "/data/vault", 3),
        meta("handbook", "work", "/data/handbook", 5),
    ];
    let actions = diff_roots(&configured, &cached);
    // Output is sorted by alias; handbook removed, vault kept.
    assert_eq!(
        actions,
        vec![
            ("handbook".into(), RootAction::RemoveRoot),
            ("vault".into(), RootAction::KeepRoot),
        ]
    );
}

#[test]
fn diff_roots_reset_when_fingerprint_changes() {
    let configured = vec![RootFingerprint {
        alias: "vault".into(),
        canonical_path: "/data/vault".into(),
        space: "personal".into(),
        include: vec!["**/*.md".into(), "**/*.txt".into()],
        exclude: vec![],
        max_file_bytes: 1024,
        // Same decoder version as the cached fingerprint so the reset is
        // attributable to the include change alone.
        decoder_version: "1".into(),
    }];
    let cached = vec![meta("vault", "personal", "/data/vault", 3)];
    let actions = diff_roots(&configured, &cached);
    assert_eq!(actions, vec![("vault".into(), RootAction::ResetRoot)]);
}

#[test]
fn fingerprint_without_decoder_version_resets_once() {
    // Fingerprints persisted before `decoder_version` existed deserialize
    // with the empty default, which never equals the current connector
    // version — so an upgraded binary re-decodes every root exactly once.
    let legacy_json = r#"{
        "alias": "vault",
        "canonical_path": "/data/vault",
        "space": "personal",
        "include": ["**/*.md"],
        "exclude": [],
        "max_file_bytes": 1024
    }"#;
    let cached_fp: RootFingerprint = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(cached_fp.decoder_version, "");

    let configured = vec![RootFingerprint {
        decoder_version: "2".into(),
        ..fp("vault", "personal", "/data/vault")
    }];
    let cached = vec![CachedRootMeta {
        alias: "vault".into(),
        space: "personal".into(),
        fingerprint: cached_fp,
        generation: 3,
    }];
    let actions = diff_roots(&configured, &cached);
    assert_eq!(actions, vec![("vault".into(), RootAction::ResetRoot)]);
}

#[test]
fn diff_roots_add_when_alias_is_new() {
    let configured = vec![fp("vault", "personal", "/data/vault")];
    let cached = vec![];
    let actions = diff_roots(&configured, &cached);
    assert_eq!(
        actions,
        vec![(
            "vault".into(),
            RootAction::KeepRoot /* first-time seen as Keep */
        )],
    );
    // First encounter is Keep with no cached generation; the apply stage is
    // responsible for inserting it. The pure planner must not invent an Add
    // variant that isn't in the enum.
}

#[test]
fn diff_roots_output_is_sorted_by_alias() {
    let configured = vec![
        fp("zeta", "s", "/z"),
        fp("alpha", "s", "/a"),
        fp("mu", "s", "/m"),
    ];
    let cached = vec![];
    let actions = diff_roots(&configured, &cached);
    let aliases: Vec<&str> = actions.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(aliases, vec!["alpha", "mu", "zeta"]);
}

// ── plan_reconcile ──────────────────────────────────────────────────────────

#[test]
fn plan_reconcile_unchanged_when_stats_and_revision_match() {
    let cached = vec![cached("a.md", 10, 100, "rev-1")];
    let observed = vec![obs("a.md", 10, 100, Some("rev-1"))];
    let actions = plan_reconcile(&cached, &observed);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], FileAction::Unchanged));
}

#[test]
fn plan_reconcile_add_when_observed_only() {
    let cached: Vec<CachedFile> = vec![];
    let observed = vec![obs("b.md", 5, 200, Some("rev-b"))];
    let actions = plan_reconcile(&cached, &observed);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        FileAction::Add(o) => {
            assert_eq!(o.locator, "b.md");
            assert_eq!(o.bytes, 5);
            assert_eq!(o.modified_ns, 200);
            assert_eq!(o.revision_hint.as_deref(), Some("rev-b"));
        }
        other => panic!("expected Add, got {other:?}"),
    }
}

#[test]
fn plan_reconcile_delete_when_cached_only() {
    let cached = vec![cached("gone.md", 7, 50, "rev-g")];
    let observed: Vec<FileObservation> = vec![];
    let actions = plan_reconcile(&cached, &observed);
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        FileAction::Delete { locator } => assert_eq!(locator, "gone.md"),
        other => panic!("expected Delete, got {other:?}"),
    }
}

#[test]
fn plan_reconcile_replace_when_revision_hint_changes() {
    let cached = vec![cached("a.md", 10, 100, "rev-old")];
    // Same bytes + modified_ns, but revision hint differs ⇒ Replace.
    let observed = vec![obs("a.md", 10, 100, Some("rev-new"))];
    let actions = plan_reconcile(&cached, &observed);
    assert_eq!(actions.len(), 1);
    assert!(
        matches!(&actions[0], FileAction::Replace(o) if o.revision_hint.as_deref() == Some("rev-new")),
        "expected Replace, got {:?}",
        actions[0]
    );
}

#[test]
fn plan_reconcile_replace_when_size_changes() {
    let cached = vec![cached("a.md", 10, 100, "rev-1")];
    let observed = vec![obs("a.md", 11, 100, Some("rev-1"))];
    let actions = plan_reconcile(&cached, &observed);
    assert!(matches!(&actions[0], FileAction::Replace(_)));
}

#[test]
fn plan_reconcile_replace_when_mtime_changes() {
    let cached = vec![cached("a.md", 10, 100, "rev-1")];
    let observed = vec![obs("a.md", 10, 101, Some("rev-1"))];
    let actions = plan_reconcile(&cached, &observed);
    assert!(matches!(&actions[0], FileAction::Replace(_)));
}

#[test]
fn plan_reconcile_output_sorted_by_locator() {
    // Mix of Unchanged (cached+observed matching) with Add, Replace, Delete
    // so each action carries a verifiable locator. Insert input in shuffled
    // order; the output must come out sorted by locator.
    let cached = vec![
        cached("zeta.md", 10, 1, "r1"), // Replace (size differs)
        cached("alpha.md", 1, 1, "r"),  // Unchanged
        cached("mu.md", 1, 1, "r-old"), // Replace (rev differs)
        cached("gone.md", 1, 1, "r"),   // Delete
    ];
    let observed = vec![
        obs("zeta.md", 11, 1, Some("r1")),
        obs("alpha.md", 1, 1, Some("r")),
        obs("mu.md", 1, 1, Some("r-new")),
        obs("new.md", 1, 1, Some("r-new")), // Add
    ];
    let actions = plan_reconcile(&cached, &observed);
    let locators: Vec<String> = actions
        .iter()
        .map(|a| match a {
            FileAction::Unchanged => "alpha.md".to_string(),
            FileAction::Add(o) => o.locator.clone(),
            FileAction::Replace(o) => o.locator.clone(),
            FileAction::Delete { locator } => locator.clone(),
            FileAction::Skip { locator, .. } => locator.clone(),
        })
        .collect();
    assert_eq!(
        locators,
        vec!["alpha.md", "gone.md", "mu.md", "new.md", "zeta.md"],
        "output must be sorted by locator"
    );
    // Sanity check on counts and kinds at the sorted indices.
    assert!(matches!(actions[0], FileAction::Unchanged));
    assert!(matches!(&actions[1], FileAction::Delete { locator } if locator == "gone.md"));
    assert!(matches!(&actions[2], FileAction::Replace(o) if o.locator == "mu.md"));
    assert!(matches!(&actions[3], FileAction::Add(o) if o.locator == "new.md"));
    assert!(matches!(&actions[4], FileAction::Replace(o) if o.locator == "zeta.md"));
}
// ── id stability ────────────────────────────────────────────────────────────

#[test]
fn document_id_is_stable_for_same_inputs() {
    let a = document_id("vault", "notes/a.md");
    let b = document_id("vault", "notes/a.md");
    assert_eq!(a, b, "document_id must be deterministic");
}

#[test]
fn document_id_differs_by_alias() {
    assert_ne!(
        document_id("vault", "notes/a.md"),
        document_id("handbook", "notes/a.md")
    );
}

#[test]
fn document_id_differs_by_locator() {
    assert_ne!(document_id("vault", "a.md"), document_id("vault", "b.md"));
}

#[test]
fn document_id_is_64_hex_chars() {
    let id = document_id("vault", "a.md");
    assert_eq!(
        id.len(),
        64,
        "blake3 hex must be 64 chars, got {}",
        id.len()
    );
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn chunk_id_is_stable_for_same_inputs() {
    let doc = document_id("vault", "notes/a.md");
    let a = chunk_id(&doc, "git:sha1:abc", 0);
    let b = chunk_id(&doc, "git:sha1:abc", 0);
    assert_eq!(a, b);
}

#[test]
fn chunk_id_differs_by_ordinal() {
    let doc = document_id("vault", "notes/a.md");
    assert_ne!(chunk_id(&doc, "rev", 0), chunk_id(&doc, "rev", 1));
}

#[test]
fn chunk_id_differs_by_revision() {
    let doc = document_id("vault", "notes/a.md");
    assert_ne!(chunk_id(&doc, "rev-old", 0), chunk_id(&doc, "rev-new", 0));
}

#[test]
fn chunk_id_differs_by_document_id() {
    assert_ne!(
        chunk_id(&document_id("vault", "a.md"), "rev", 0),
        chunk_id(&document_id("vault", "b.md"), "rev", 0),
    );
}

#[test]
fn chunk_id_is_64_hex_chars() {
    let id = chunk_id(&document_id("vault", "a.md"), "rev", 0);
    assert_eq!(id.len(), 64);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
}

// ── proptest: conservation ─────────────────────────────────────────────────

fn arb_locator() -> impl Strategy<Value = String> {
    "[a-z0-9_/\\.]{1,16}"
}

fn arb_obs() -> impl Strategy<Value = FileObservation> {
    (
        arb_locator(),
        0u64..4096,
        0i64..1_000_000,
        proptest::option::of("[a-z0-9]{1,8}"),
    )
        .prop_map(
            |(locator, bytes, modified_ns, revision_hint)| FileObservation {
                locator,
                bytes,
                modified_ns,
                revision_hint,
            },
        )
}

fn arb_cached() -> impl Strategy<Value = CachedFile> {
    (arb_locator(), 0u64..4096, 0i64..1_000_000, "[a-z0-9]{1,8}").prop_map(
        |(locator, bytes, modified_ns, revision)| CachedFile {
            locator,
            bytes,
            modified_ns,
            revision,
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Conservation: every cached and every observed locator appears exactly
    /// once across the output. No duplicates, no losses.
    #[test]
    fn prop_plan_reconcile_conservation(
        cached in proptest::collection::vec(arb_cached(), 0..32),
        observed in proptest::collection::vec(arb_obs(), 0..32),
    ) {
        let actions = plan_reconcile(&cached, &observed);

        // Expected set: union of unique locators from both inputs.
        let mut expected: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for c in &cached {
            expected.insert(c.locator.clone());
        }
        for o in &observed {
            expected.insert(o.locator.clone());
        }

        let actual: std::collections::BTreeSet<String> = actions
            .iter()
            .map(|a| match a {
                FileAction::Unchanged => String::new(), // placeholder; handled below
                FileAction::Add(o) => o.locator.clone(),
                FileAction::Replace(o) => o.locator.clone(),
                FileAction::Delete { locator } => locator.clone(),
                FileAction::Skip { locator, .. } => locator.clone(),
            })
            // Unchanged doesn't carry a locator in its variant; rebuild the
            // set by intersecting cached/observed instead.
            .collect();

        // Unchanged contributes every locator that has an Unchanged action.
        let unchanged_locators: std::collections::BTreeSet<String> = cached
            .iter()
            .filter(|c| {
                let rev = c.revision.clone();
                let hit = observed.iter().find(|o| {
                    o.locator == c.locator
                        && o.bytes == c.bytes
                        && o.modified_ns == c.modified_ns
                        && o.revision_hint.as_deref() == Some(rev.as_str())
                });
                hit.is_some()
            })
            .map(|c| c.locator.clone())
            .collect();

        let mut expected_with_unchanged = expected.clone();
        for l in &unchanged_locators {
            expected_with_unchanged.insert(l.clone());
        }

        let mut actual_with_unchanged = actual.clone();
        for l in &unchanged_locators {
            actual_with_unchanged.insert(l.clone());
        }

        // No duplicates across all actions (incl. Unchanged).
        let all_locators: Vec<String> = actions
            .iter()
            .map(|a| match a {
                FileAction::Unchanged => {
                    // Pick the first cached locator that is Unchanged; multiple
                    // Unchanged entries are fine as long as their locators
                    // don't collide elsewhere. We re-derive from cached/observed.
                    String::new()
                }
                FileAction::Add(o) => o.locator.clone(),
                FileAction::Replace(o) => o.locator.clone(),
                FileAction::Delete { locator } => locator.clone(),
                FileAction::Skip { locator, .. } => locator.clone(),
            })
            .collect();

        let unique_non_unchanged: std::collections::BTreeSet<String> =
            all_locators.iter().filter(|s| !s.is_empty()).cloned().collect();
        prop_assert_eq!(
            unique_non_unchanged.len(),
            all_locators.iter().filter(|s| !s.is_empty()).count(),
            "no duplicate non-Unchanged locators"
        );

        prop_assert_eq!(
            unique_non_unchanged,
            expected_with_unchanged,
            "every cached + observed locator must appear exactly once"
        );
    }

    /// Determinism: same inputs ⇒ same outputs (byte-equal via serde).
    #[test]
    fn prop_plan_reconcile_deterministic(
        cached in proptest::collection::vec(arb_cached(), 0..16),
        observed in proptest::collection::vec(arb_obs(), 0..16),
    ) {
        let a = plan_reconcile(&cached, &observed);
        let b = plan_reconcile(&cached, &observed);
        prop_assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
    }
}

proptest! {
    #[test]
    fn heading_sections_preserve_descendants_and_end_at_the_next_peer(
        pre in "[a-z ]{0,100}", body in "[a-z가-힣 ]{1,200}", tail in "[a-z ]{0,100}",
    ) {
        use oxibrain_core::documents::{DocumentHeading, recall_document_range};
        let text = format!("{pre}\n# Target\n{body}\n## Child\ncondition\n# Other\n{tail}");
        let start = text.find("# Target").unwrap();
        let child = text.find("## Child").unwrap();
        let end = text.find("# Other").unwrap();
        let headings = vec![
            DocumentHeading {start, end: start + 8, level: 1},
            DocumentHeading {start: child, end: child + 8, level: 2},
            DocumentHeading {start: end, end: end + 7, level: 1},
        ];
        let range = recall_document_range(&text, &headings, start..start+9, "Target", 4096)
            .unwrap();
        prop_assert_eq!(&range, &(start..end));
        prop_assert!(text[range].contains("condition"));
    }
}

#[test]
fn body_hits_expand_to_the_enclosing_section_without_heading_query_overlap() {
    use oxibrain_core::documents::{DocumentHeading, recall_document_range};
    let text = "# Recovery\nRun Kestrel-841 after validation.\n## Guard\nOnly when health-check is red.\n### Detail\nKeep the audit note.\n# Neighbor\nIgnore this section.";
    let recovery = text.find("# Recovery").unwrap();
    let guard = text.find("## Guard").unwrap();
    let detail = text.find("### Detail").unwrap();
    let neighbor = text.find("# Neighbor").unwrap();
    let headings = vec![
        DocumentHeading {
            start: recovery,
            end: recovery + 10,
            level: 1,
        },
        DocumentHeading {
            start: guard,
            end: guard + 8,
            level: 2,
        },
        DocumentHeading {
            start: detail,
            end: detail + 10,
            level: 3,
        },
        DocumentHeading {
            start: neighbor,
            end: neighbor + 10,
            level: 1,
        },
    ];
    let command = text.find("Kestrel-841").unwrap();
    let range = recall_document_range(
        text,
        &headings,
        command..command + 11,
        "unrelated query words",
        8,
    )
    .unwrap();
    assert_eq!(range, recovery..neighbor);
    assert!(text[range.clone()].contains("health-check is red"));
    assert!(text[range.clone()].contains("Keep the audit note"));
    assert!(!text[range].contains("Ignore this section"));

    let condition = text.find("health-check").unwrap();
    let child = recall_document_range(
        text,
        &headings,
        condition..condition + 11,
        "no heading overlap",
        8,
    )
    .unwrap();
    assert_eq!(child, recovery..neighbor);
    assert!(text[child].contains("Only when health-check is red"));
}

#[test]
fn heading_documents_refuse_unsafe_preamble_fallback_and_cross_peer_hits() {
    use oxibrain_core::documents::{
        DocumentHeading, recall_document_range, recall_document_ranges,
    };
    let text = "Preamble command Kestrel-841.\n# One\nbody one\n# Two\nbody two";
    let one = text.find("# One").unwrap();
    let two = text.find("# Two").unwrap();
    let headings = vec![
        DocumentHeading {
            start: one,
            end: one + 5,
            level: 1,
        },
        DocumentHeading {
            start: two,
            end: two + 5,
            level: 1,
        },
    ];
    let preamble_hit = text.find("Kestrel-841").unwrap();
    assert!(
        recall_document_range(
            text,
            &headings,
            preamble_hit..preamble_hit + 11,
            "Kestrel",
            4
        )
        .is_none()
    );
    assert!(recall_document_range(text, &headings, one..text.len(), "missing-key", 4).is_none());

    // Both peers share lexical signal. Keep each complete section rather than
    // treating fewer query anchors as proof that the other can be discarded.
    let hits = recall_document_ranges(text, &headings, one..text.len(), "body-two", 4).unwrap();
    assert_eq!(hits, vec![one..two, two..text.len()]);
    assert!(text[hits[1].clone()].contains("body two"));
    assert!(recall_document_range(text, &headings, one..text.len(), "body-two", 4).is_none());
}

#[test]
fn invalid_heading_offsets_allow_only_a_whole_view_document_unit() {
    use oxibrain_core::evidence::{SourceCoordinate, document_recall_unit};
    let text = "# Recovery\nRun Kestrel-841 only after validation.";
    let coordinate = |start, end| SourceCoordinate {
        reference: "doc://vault/note.md?rev=one".into(),
        revision: "one".into(),
        view: "decoded-utf8/1".into(),
        view_digest: Some("digest".into()),
        decoder: Some("markdown/1".into()),
        start,
        end,
        document_uuid: None,
    };
    let body = text.find("Kestrel-841").unwrap();
    assert!(document_recall_unit(text, None, coordinate(body, body + 11), "Kestrel", 8).is_none());

    let whole = document_recall_unit(text, None, coordinate(0, text.len()), "Kestrel", 8).unwrap();
    assert_eq!(whole.text, text);
    assert!(whole.variants.is_empty());
}

#[test]
fn a_whole_document_hit_keeps_distinct_peer_query_anchors() {
    use oxibrain_core::documents::{DocumentHeading, recall_document_ranges};

    let text = "# Kestrel-841\nOnly when the health-check is red.\n# Birch-26\nOnly after validation fails.\n# Archive\nHow the old release worked.";
    let first = text.find("# Kestrel-841").unwrap();
    let second = text.find("# Birch-26").unwrap();
    let third = text.find("# Archive").unwrap();
    let headings = vec![
        DocumentHeading {
            start: first,
            end: first + "# Kestrel-841".len(),
            level: 1,
        },
        DocumentHeading {
            start: second,
            end: second + "# Birch-26".len(),
            level: 1,
        },
        DocumentHeading {
            start: third,
            end: third + "# Archive".len(),
            level: 1,
        },
    ];
    let ranges = recall_document_ranges(
        text,
        &headings,
        0..text.len(),
        "Compare Kestrel-841 and Birch-26 with each condition",
        8,
    )
    .unwrap();
    assert!(ranges.contains(&(first..second)));
    assert!(ranges.contains(&(second..third)));
    // P10: lexical rank can find the requested sections first, but cannot prove
    // a third peer irrelevant. Verify target ranges stay section-scoped.
    for range in ranges
        .iter()
        .filter(|range| range.start == first || range.start == second)
    {
        assert!(text[range.clone()].contains("Only"));
        assert!(!text[range.clone()].contains("Archive"));
    }
}

#[test]
fn shared_generic_version_terms_keep_both_conditional_branches() {
    use oxibrain_core::documents::{DocumentHeading, recall_document_ranges};

    let text = "# Release version 5.1\nOnly when the cluster is active.\n# Release version 5.2\nOnly when the device is offline.";
    let first = text.find("# Release version 5.1").unwrap();
    let second = text.find("# Release version 5.2").unwrap();
    let headings = vec![
        DocumentHeading {
            start: first,
            end: first + "# Release version 5.1".len(),
            level: 1,
        },
        DocumentHeading {
            start: second,
            end: second + "# Release version 5.2".len(),
            level: 1,
        },
    ];
    let ranges = recall_document_ranges(
        text,
        &headings,
        0..text.len(),
        "Which release version applies?",
        8,
    )
    .unwrap();
    assert_eq!(ranges, vec![first..second, second..text.len()]);
    assert!(text[ranges[0].clone()].contains("cluster is active"));
    assert!(text[ranges[1].clone()].contains("device is offline"));
}

#[test]
fn document_section_selection_preserves_global_retrieval_order() {
    use oxibrain_core::context::LayerKind;
    use oxibrain_core::evidence::{
        DocumentRecallCandidate, EvidenceUnit, SourceCoordinate, select_document_recall_units,
    };

    let candidate = |reference: &str, score| DocumentRecallCandidate {
        unit: EvidenceUnit::new(
            LayerKind::Documents,
            "Recovery command applies only after validation.".into(),
            SourceCoordinate {
                reference: reference.into(),
                revision: "revision-1".into(),
                view: "decoded-utf8/1".into(),
                view_digest: Some("digest".into()),
                decoder: Some("markdown/1".into()),
                start: 0,
                end: "Recovery command applies only after validation.".len(),
                document_uuid: None,
            },
            "untrusted_observation",
        ),
        retrieval_score: score,
    };
    let selected = select_document_recall_units(
        "recovery command",
        vec![
            candidate("doc://a/low?rev=1", 0.1),
            candidate("doc://z/high?rev=1", 0.9),
        ],
    );
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].source.reference, "doc://z/high?rev=1");
    assert_eq!(selected[1].source.reference, "doc://a/low?rev=1");
}

#[test]
fn exact_command_match_does_not_drop_a_peer_exception_condition() {
    use oxibrain_core::documents::{DocumentHeading, recall_document_ranges};

    // P10: a stronger command match does not prove an independently matching
    // exception is redundant; keep both conditions in the evidence set.
    let text = "# Procedure\nRestore command: run restore-now.\n# Exception\nDo not restore while the standby is unhealthy.";
    let procedure = text.find("# Procedure").unwrap();
    let exception = text.find("# Exception").unwrap();
    let headings = vec![
        DocumentHeading {
            start: procedure,
            end: procedure + "# Procedure".len(),
            level: 1,
        },
        DocumentHeading {
            start: exception,
            end: exception + "# Exception".len(),
            level: 1,
        },
    ];
    let ranges =
        recall_document_ranges(text, &headings, 0..text.len(), "restore command", 8).unwrap();
    assert_eq!(ranges, vec![procedure..exception, exception..text.len()]);
    assert!(text[ranges[0].clone()].contains("restore-now"));
    assert!(text[ranges[1].clone()].contains("Do not restore while the standby is unhealthy"));
}

#[test]
fn candidate_ranking_keeps_an_opposing_exception_under_an_exact_query() {
    use oxibrain_core::context::LayerKind;
    use oxibrain_core::evidence::{
        DocumentRecallCandidate, EvidenceUnit, SourceCoordinate, select_document_recall_units,
    };

    let reference = "doc://vault/procedure.md?rev=1";
    let document = "# Procedure\nRestore command: run restore-now.\n# Exception\nDo not restore while the standby is unhealthy.";
    let procedure = document.find("# Procedure").unwrap();
    let exception = document.find("# Exception").unwrap();
    let candidate = |range: std::ops::Range<usize>, score| {
        let text = &document[range.clone()];
        DocumentRecallCandidate {
            unit: EvidenceUnit::new(
                LayerKind::Documents,
                text.into(),
                SourceCoordinate {
                    reference: reference.into(),
                    revision: "revision-1".into(),
                    view: "decoded-utf8/1".into(),
                    view_digest: Some("digest".into()),
                    decoder: Some("markdown/1".into()),
                    start: range.start,
                    end: range.end,
                    document_uuid: None,
                },
                "untrusted_observation",
            ),
            retrieval_score: score,
        }
    };
    let selected = select_document_recall_units(
        "restore command",
        vec![
            candidate(procedure..exception, 1.0),
            candidate(exception..document.len(), 0.8),
        ],
    );
    assert_eq!(selected.len(), 2);
    assert!(
        selected
            .iter()
            .any(|unit| unit.text.contains("restore-now"))
    );
    assert!(
        selected
            .iter()
            .any(|unit| unit.text.contains("Do not restore"))
    );
}
