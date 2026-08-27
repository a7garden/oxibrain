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
    }];
    let cached = vec![meta("vault", "personal", "/data/vault", 3)];
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
