//! Vault sync classification — a pure decision function (P9).
//!
//! Given scanned markdown files and the ledger's known note content hashes per
//! source path, decide per file whether sync must ingest it (`New`), skip it
//! (`Unchanged` — an episode with this exact content already exists), or ingest
//! it as a new version (`Modified` — the path is known with different content).
//!
//! The store fetches the `KnownNotes` map; this module only decides. Modified
//! files append a new episode; the previous episode remains (append-only
//! ledger, P1). Stale claims surface via `contradictions` and are removed with
//! `retract` — sync itself never retracts.

use crate::types::ContentHash;
use oxibrain_ports::Timestamp;
use std::collections::{HashMap, HashSet};

/// A scanned candidate file for sync.
///
/// `path` is relative to the sync root with forward slashes (stable across
/// machines and working directories). `modified` is the file's mtime and
/// becomes the episode's `occurred_at`, so episode ids are stable across
/// re-syncs of an unchanged tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFile {
    pub path: String,
    pub content_hash: ContentHash,
    pub modified: Timestamp,
}

/// What sync must do with one scanned file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// No live episode for this path — ingest.
    New(SyncFile),
    /// An episode with this exact content exists — skip (idempotent no-op).
    Unchanged(String),
    /// The path is known with different content — ingest as a new episode.
    /// The previous episode remains (P1); see the module docs.
    Modified(SyncFile),
}

/// Known note content hashes per source path, as read from the ledger
/// (`store::ledger::note_hashes_by_path`).
pub type KnownNotes = HashMap<String, HashSet<ContentHash>>;

/// Classify scanned files against known notes.
///
/// Pure and total: every input file appears in exactly one output action
/// (conservation), and the output preserves the input order (callers pass the
/// scan's deterministic path order).
pub fn classify(files: Vec<SyncFile>, known: &KnownNotes) -> Vec<SyncAction> {
    files
        .into_iter()
        .map(|f| match known.get(&f.path) {
            // An empty set means no live episode for the path (e.g. all
            // versions redacted) — same decision as an unknown path.
            Some(hashes) if !hashes.is_empty() => {
                if hashes.contains(&f.content_hash) {
                    SyncAction::Unchanged(f.path)
                } else {
                    SyncAction::Modified(f)
                }
            }
            _ => SyncAction::New(f),
        })
        .collect()
}

/// State of the latest event-path episode for a locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatorState {
    /// The occurrence_id of the most recent episode for this locator.
    pub latest_occurrence_id: String,
    /// The content hash of that episode.
    pub latest_content_hash: ContentHash,
}

/// Classify scanned files using event-identity state, falling back to legacy
/// content-hash knowledge for locators not yet on the event path.
///
/// Precedence: event_states > legacy > New.
/// Pure and total: every input file appears in exactly one output action.
pub fn classify_event(
    files: Vec<SyncFile>,
    legacy: &KnownNotes,
    event_states: &HashMap<String, LocatorState>,
) -> Vec<SyncAction> {
    files
        .into_iter()
        .map(|f| {
            if let Some(state) = event_states.get(&f.path) {
                if state.latest_content_hash == f.content_hash {
                    SyncAction::Unchanged(f.path)
                } else {
                    SyncAction::Modified(f)
                }
            } else if let Some(hashes) = legacy.get(&f.path) {
                if !hashes.is_empty() && hashes.contains(&f.content_hash) {
                    SyncAction::Unchanged(f.path)
                } else {
                    SyncAction::Modified(f)
                }
            } else {
                SyncAction::New(f)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_hash;
    use proptest::prelude::*;

    fn file(path: &str, content: &str, t: i64) -> SyncFile {
        SyncFile {
            path: path.into(),
            content_hash: content_hash(content),
            modified: Timestamp(t),
        }
    }

    fn known(entries: &[(&str, &[&str])]) -> KnownNotes {
        entries
            .iter()
            .map(|(path, contents)| {
                (
                    (*path).to_string(),
                    contents.iter().map(|c| content_hash(c)).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn unknown_path_is_new() {
        let actions = classify(vec![file("a.md", "hello", 1)], &known(&[]));
        assert_eq!(actions, vec![SyncAction::New(file("a.md", "hello", 1))]);
    }

    #[test]
    fn matching_hash_is_unchanged() {
        let actions = classify(
            vec![file("a.md", "hello", 2)],
            &known(&[("a.md", &["hello"])]),
        );
        assert_eq!(actions, vec![SyncAction::Unchanged("a.md".into())]);
    }

    #[test]
    fn mismatched_hash_is_modified() {
        let actions = classify(
            vec![file("a.md", "hello v2", 2)],
            &known(&[("a.md", &["hello"])]),
        );
        assert_eq!(
            actions,
            vec![SyncAction::Modified(file("a.md", "hello v2", 2))]
        );
    }

    #[test]
    fn any_prior_version_hash_counts_as_unchanged() {
        // The path has two versions ingested already; current content matches
        // the older one exactly — still a no-op (content identity, not recency).
        let actions = classify(
            vec![file("a.md", "hello", 3)],
            &known(&[("a.md", &["hello", "hello v2"])]),
        );
        assert_eq!(actions, vec![SyncAction::Unchanged("a.md".into())]);
    }

    #[test]
    fn empty_known_entry_is_new() {
        // A path with no live episodes (e.g. all redacted) is New, not Modified.
        let mut map = KnownNotes::new();
        map.insert("a.md".into(), HashSet::new());
        let actions = classify(vec![file("a.md", "hello", 1)], &map);
        assert_eq!(actions, vec![SyncAction::New(file("a.md", "hello", 1))]);
    }

    #[test]
    fn output_preserves_input_order() {
        let files = vec![
            file("b.md", "b", 1),
            file("a.md", "a", 1),
            file("c.md", "c", 1),
        ];
        let actions = classify(files, &known(&[]));
        let paths: Vec<&str> = actions
            .iter()
            .map(|a| match a {
                SyncAction::New(f) | SyncAction::Modified(f) => f.path.as_str(),
                SyncAction::Unchanged(p) => p.as_str(),
            })
            .collect();
        assert_eq!(paths, vec!["b.md", "a.md", "c.md"]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Conservation and classification law: for arbitrary inputs, every
        /// file lands in exactly one action, and the action agrees with the
        /// known map per the module rules.
        #[test]
        fn classify_is_total_and_correct(
            files in proptest::collection::vec(
                (".*a?b?c?[0-9]{0,3}\\.md", ".*", 0i64..1000),
                0..16
            ),
            known in proptest::collection::vec(
                (".*a?b?c?[0-9]{0,3}\\.md", proptest::collection::vec(".*", 0..3)),
                0..8
            ),
        ) {
            let sync_files: Vec<SyncFile> = files
                .iter()
                .map(|(p, c, t)| file(p, c, *t))
                .collect();
            let mut map = KnownNotes::new();
            for (p, cs) in &known {
                let set: HashSet<ContentHash> = cs.iter().map(|c| content_hash(c)).collect();
                map.insert(p.clone(), set);
            }
            let actions = classify(sync_files.clone(), &map);

            // Conservation: one action per input, same order.
            assert_eq!(actions.len(), sync_files.len());
            for (f, a) in sync_files.iter().zip(&actions) {
                let got_path = match a {
                    SyncAction::New(sf) | SyncAction::Modified(sf) => &sf.path,
                    SyncAction::Unchanged(p) => p,
                };
                assert_eq!(got_path, &f.path);
                // Classification law.
                let entry = map.get(&f.path);
                let expect_unchanged = entry.is_some_and(|s| s.contains(&f.content_hash));
                let expect_new = entry.is_none_or(|s| s.is_empty());
                match a {
                    SyncAction::Unchanged(_) => assert!(expect_unchanged),
                    SyncAction::New(_) => assert!(!expect_unchanged && expect_new),
                    SyncAction::Modified(_) => assert!(!expect_unchanged && !expect_new),
                }
            }
        }
    }

    use super::LocatorState;

    fn locator_state(occ: &str, content: &str) -> LocatorState {
        LocatorState {
            latest_occurrence_id: occ.into(),
            latest_content_hash: content_hash(content),
        }
    }

    #[test]
    fn classify_event_new_when_no_state() {
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &KnownNotes::new(), &HashMap::new());
        assert_eq!(actions, vec![SyncAction::New(file("a.md", "hello", 1))]);
    }

    #[test]
    fn classify_event_unchanged_when_event_hash_matches() {
        let states = HashMap::from([("a.md".to_string(), locator_state("occ1", "hello"))]);
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &KnownNotes::new(), &states);
        assert_eq!(actions, vec![SyncAction::Unchanged("a.md".into())]);
    }

    #[test]
    fn classify_event_modified_when_event_hash_differs() {
        let states = HashMap::from([("a.md".to_string(), locator_state("occ1", "old"))]);
        let files = vec![file("a.md", "new", 2)];
        let actions = classify_event(files, &KnownNotes::new(), &states);
        assert_eq!(actions, vec![SyncAction::Modified(file("a.md", "new", 2))]);
    }

    #[test]
    fn classify_event_unchanged_via_legacy_hash() {
        // No event-path state, but legacy hash matches → Unchanged.
        let mut legacy = KnownNotes::new();
        legacy.insert("a.md".into(), HashSet::from([content_hash("hello")]));
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &legacy, &HashMap::new());
        assert_eq!(actions, vec![SyncAction::Unchanged("a.md".into())]);
    }

    #[test]
    fn classify_event_modified_via_legacy_mismatch() {
        // Legacy knows the path but content changed → Modified (first event-path ingest).
        let mut legacy = KnownNotes::new();
        legacy.insert("a.md".into(), HashSet::from([content_hash("old")]));
        let files = vec![file("a.md", "new", 2)];
        let actions = classify_event(files, &legacy, &HashMap::new());
        assert_eq!(actions, vec![SyncAction::Modified(file("a.md", "new", 2))]);
    }

    #[test]
    fn classify_event_event_state_takes_precedence_over_legacy() {
        // Event state says "old", legacy says "hello" — event state wins.
        let states = HashMap::from([("a.md".to_string(), locator_state("occ1", "old"))]);
        let mut legacy = KnownNotes::new();
        legacy.insert("a.md".into(), HashSet::from([content_hash("hello")]));
        let files = vec![file("a.md", "hello", 1)];
        let actions = classify_event(files, &legacy, &states);
        // Event state hash != file hash → Modified, even though legacy matches.
        assert_eq!(
            actions,
            vec![SyncAction::Modified(file("a.md", "hello", 1))]
        );
    }
}
