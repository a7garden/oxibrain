//! Bounded, language-neutral lexical query planning and candidate fusion.
//!
//! This module makes no assumptions about word lists, scripts, stemming, or
//! corpus language. It preserves the full query for exact matching, then adds
//! a bounded set of longer whitespace-delimited terms and Unicode trigrams.

const MAX_TERMS: usize = 8;
const MAX_TERM_CHARS: usize = 64;
const MAX_TRIGRAMS: usize = 12;
const MAX_PLAN_CHARS: usize = 4096;

pub const LEXICAL_QUERY_PLAN_VERSION: &str = "lexical-query-plan/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalQueryPlan {
    /// The caller's complete, unmodified query. The index tokenizer handles
    /// punctuation and case while quoted FTS terms prevent syntax injection.
    pub exact: String,
    /// Longer query terms are kept in original order after breadth-independent
    /// selection. These remain byte-preserving so identifiers are not altered.
    pub terms: Vec<String>,
    /// Case-folded Unicode scalar trigrams sampled across selected terms.
    pub trigrams: Vec<String>,
    pub terms_capped: bool,
    pub trigrams_capped: bool,
    pub term_limit: usize,
    pub trigram_limit: usize,
    pub terms_examined: usize,
    /// Distinct trigrams found in the bounded set of selected query terms.
    pub trigrams_examined: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalCandidateKind {
    ExactQuery,
    Term,
    Trigram,
    Dense,
}

/// Language-neutral overlap used to choose among sibling document sections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct LexicalRelevance {
    pub exact_query: bool,
    pub term_weight: usize,
    pub trigram_overlap: usize,
}

/// One independently fetched candidate list. `breadth` is the store's total
/// eligible match count before its result limit, used to discount common terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalCandidateChannel {
    pub kind: LexicalCandidateKind,
    pub ids: Vec<String>,
    pub breadth: usize,
}

impl LexicalQueryPlan {
    pub fn new(query: &str) -> Self {
        let bounded_query: String = query.chars().take(MAX_PLAN_CHARS).collect();
        let words: Vec<String> = bounded_query
            .split_whitespace()
            .filter(|word| !word.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        let terms_examined = words.len();

        // Longer tokens tend to be more discriminative without introducing
        // language-specific stop lists or term-frequency assumptions. Stable
        // source order breaks equal-length ties.
        let mut ranked: Vec<(usize, String)> = words
            .iter()
            .enumerate()
            .filter(|(_, word)| word.chars().count() <= MAX_TERM_CHARS)
            .map(|(index, word)| (index, word.clone()))
            .collect();
        ranked.sort_by(|(ai, a), (bi, b)| {
            b.chars()
                .count()
                .cmp(&a.chars().count())
                .then_with(|| ai.cmp(bi))
        });

        let mut selected = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (index, word) in &ranked {
            if selected.len() == MAX_TERMS {
                break;
            }
            let key: String = word.chars().flat_map(char::to_lowercase).collect();
            if seen.insert(key) {
                selected.push((*index, word.clone()));
            }
        }
        selected.sort_by_key(|(index, _)| *index);
        let terms: Vec<String> = selected.iter().map(|(_, word)| word.clone()).collect();
        let eligible_terms: std::collections::BTreeSet<String> = words
            .iter()
            .filter(|word| word.chars().count() <= MAX_TERM_CHARS)
            .map(|word| word.chars().flat_map(char::to_lowercase).collect())
            .collect();
        let terms_capped = eligible_terms.len() > terms.len()
            || words
                .iter()
                .any(|word| word.chars().count() > MAX_TERM_CHARS)
            || bounded_query != query;

        let mut per_term = Vec::<Vec<String>>::new();
        let mut all = std::collections::BTreeSet::new();
        let mut total_trigrams = 0usize;
        let mut trigram_terms: Vec<_> = words.iter().collect();
        trigram_terms.sort_by(|a, b| {
            b.chars()
                .count()
                .cmp(&a.chars().count())
                .then_with(|| a.cmp(b))
        });
        let mut trigram_seen = std::collections::BTreeSet::new();
        for term in trigram_terms {
            let term_key: String = term.chars().flat_map(char::to_lowercase).collect();
            if !trigram_seen.insert(term_key) {
                continue;
            }
            if per_term.len() == MAX_TERMS {
                break;
            }
            let folded: String = term.chars().flat_map(char::to_lowercase).collect();
            let mut term_seen = std::collections::BTreeSet::new();
            let grams: Vec<String> = folded
                .chars()
                .collect::<Vec<_>>()
                .windows(3)
                .map(|window| window.iter().copied().collect())
                .filter(|gram: &String| term_seen.insert(gram.clone()))
                .collect();
            for gram in &grams {
                if all.insert(gram.clone()) {
                    total_trigrams += 1;
                }
            }
            per_term.push(sample_trigrams(grams, MAX_TRIGRAMS));
        }

        // First represent each selected term, then round-robin the remaining
        // distinct trigrams. This prevents one long token from consuming the
        // entire trigram budget and works identically across writing systems.
        let mut trigrams = Vec::new();
        let mut chosen = std::collections::BTreeSet::new();
        for grams in &per_term {
            if trigrams.len() == MAX_TRIGRAMS {
                break;
            }
            if let Some(gram) = grams.get(grams.len() / 2) {
                if chosen.insert(gram.clone()) {
                    trigrams.push(gram.clone());
                }
            }
        }
        let mut offset = 0;
        while trigrams.len() < MAX_TRIGRAMS {
            for grams in &per_term {
                if let Some(gram) = grams.get(offset) {
                    if chosen.insert(gram.clone()) {
                        trigrams.push(gram.clone());
                        if trigrams.len() == MAX_TRIGRAMS {
                            break;
                        }
                    }
                }
            }
            if per_term.iter().all(|grams| offset >= grams.len()) {
                break;
            }
            offset += 1;
        }
        let unique_trigram_terms: std::collections::BTreeSet<String> = words
            .iter()
            .map(|word| word.chars().flat_map(char::to_lowercase).collect())
            .collect();
        let trigrams_capped = total_trigrams > trigrams.len()
            || unique_trigram_terms.len() > MAX_TERMS
            || bounded_query != query;

        Self {
            exact: query.to_owned(),
            terms,
            trigrams,
            terms_capped,
            trigrams_capped,
            term_limit: MAX_TERMS,
            trigram_limit: MAX_TRIGRAMS,
            terms_examined,
            trigrams_examined: total_trigrams,
        }
    }

    pub fn relevance(&self, text: &str) -> LexicalRelevance {
        let folded = text.to_lowercase();
        LexicalRelevance {
            exact_query: !self.exact.trim().is_empty()
                && folded.contains(&self.exact.to_lowercase()),
            term_weight: self
                .terms
                .iter()
                .filter(|term| folded.contains(&term.to_lowercase()))
                .map(|term| term.chars().count())
                .sum(),
            trigram_overlap: self
                .trigrams
                .iter()
                .filter(|gram| folded.contains(gram.as_str()))
                .count(),
        }
    }
}

fn sample_trigrams(grams: Vec<String>, maximum: usize) -> Vec<String> {
    if grams.len() <= maximum || maximum == 0 {
        return grams;
    }
    if maximum == 1 {
        return vec![grams[grams.len() / 2].clone()];
    }
    (0..maximum)
        .map(|i| grams[i * (grams.len() - 1) / (maximum - 1)].clone())
        .collect()
}

/// Deterministically fuse lexical channels, discounting broad terms by their
/// observed eligible-match count. Each channel's rank is normalized separately
/// so raw BM25 magnitude or a common token cannot dominate a narrow identifier.
pub fn fuse_lexical_candidates(
    channels: &[LexicalCandidateChannel],
    maximum: usize,
) -> (Vec<(String, f64)>, Vec<String>) {
    let term_count = channels
        .iter()
        .filter(|channel| channel.kind == LexicalCandidateKind::Term)
        .count()
        .max(1);
    let trigram_count = channels
        .iter()
        .filter(|channel| channel.kind == LexicalCandidateKind::Trigram)
        .count()
        .max(1);
    let mut scores = std::collections::BTreeMap::<String, f64>::new();
    for channel in channels {
        let breadth_weight = 1.0 / (1.0 + channel.breadth as f64).sqrt();
        let kind_weight = match channel.kind {
            LexicalCandidateKind::ExactQuery => 2.0,
            LexicalCandidateKind::Term => 1.0 / (term_count as f64).sqrt(),
            LexicalCandidateKind::Trigram => 0.5 / (trigram_count as f64).sqrt(),
            LexicalCandidateKind::Dense => 1.0,
        };
        let mut seen = std::collections::BTreeSet::new();
        for (rank, id) in channel.ids.iter().enumerate() {
            if seen.insert(id) {
                *scores.entry(id.clone()).or_default() +=
                    kind_weight * breadth_weight / (61 + rank) as f64;
            }
        }
    }
    let mut rows: Vec<_> = scores.into_iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut selected = rows;
    let dropped_rows = selected.split_off(maximum.min(selected.len()));
    let dropped = dropped_rows.into_iter().map(|(id, _)| id).collect();
    (selected, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn preserves_query_and_identifier_while_bounding_unicode_candidates() {
        let query = "Which restore-release-53 condition applies? 배포-상태 확인";
        let plan = LexicalQueryPlan::new(query);
        assert_eq!(plan.exact, query);
        assert!(plan.terms.contains(&"restore-release-53".to_owned()));
        assert!(plan.terms.len() <= MAX_TERMS);
        assert!(plan.trigrams.len() <= MAX_TRIGRAMS);
        assert!(plan.trigrams.iter().all(|gram| gram.chars().count() == 3));
        assert!(plan.trigrams.iter().any(|gram| gram.contains('배')));
    }

    #[test]
    fn trigram_plan_handles_unspaced_and_arbitrary_writing_system_text() {
        let plan = LexicalQueryPlan::new("条件付き復元命令の確認");
        assert!(!plan.trigrams.is_empty());
        assert!(plan.trigrams.iter().all(|gram| gram.chars().count() == 3));
        assert!(plan.trigrams.len() <= MAX_TRIGRAMS);

        let affixed = LexicalQueryPlan::new("条件付き復元命令を");
        let document_grams: std::collections::BTreeSet<String> = "条件付き復元命令"
            .chars()
            .collect::<Vec<_>>()
            .windows(3)
            .map(|window| window.iter().copied().collect())
            .collect();
        assert!(
            affixed
                .trigrams
                .iter()
                .any(|gram| document_grams.contains(gram))
        );

        let long = format!("{}qzx", "条件付き復元命令".repeat(16));
        let long_plan = LexicalQueryPlan::new(&long);
        assert!(long_plan.terms.is_empty());
        assert!(long_plan.terms_capped);
        assert!(!long_plan.trigrams.is_empty());
        assert!(long_plan.trigrams.iter().any(|gram| gram == "qzx"));
        assert_eq!(long_plan.trigrams_examined, 11);
        assert_eq!(long_plan.trigrams_examined, long_plan.trigrams.len());
        assert!(!long_plan.trigrams_capped);
    }

    #[test]
    fn broad_common_term_does_not_overpower_narrow_identifier_match() {
        let channels = vec![
            LexicalCandidateChannel {
                kind: LexicalCandidateKind::Term,
                ids: vec!["decoy".into(), "target".into()],
                breadth: 10_000,
            },
            LexicalCandidateChannel {
                kind: LexicalCandidateKind::Term,
                ids: vec!["target".into()],
                breadth: 1,
            },
            LexicalCandidateChannel {
                kind: LexicalCandidateKind::ExactQuery,
                ids: vec!["target".into()],
                breadth: 1,
            },
        ];
        let (selected, dropped) = fuse_lexical_candidates(&channels, 1);
        assert_eq!(selected[0].0.as_str(), "target");
        assert_eq!(dropped, vec!["decoy".to_owned()]);
    }

    proptest! {
        #[test]
        fn keeps_all_distinct_trigrams_when_the_plan_is_under_its_limit(
            left in "[a-z]{5}",
            right in "[a-z]{5}",
        ) {
            let expected: std::collections::BTreeSet<String> = [left.as_str(), right.as_str()]
                .into_iter()
                .flat_map(|term| {
                    term.as_bytes()
                        .windows(3)
                        .map(|window| String::from_utf8(window.to_vec()).expect("ASCII trigram"))
                })
                .collect();
            prop_assert!(expected.len() <= MAX_TRIGRAMS);
            let query = format!("{left} {right}");
            let plan = LexicalQueryPlan::new(&query);
            prop_assert_eq!(
                plan.trigrams.iter().cloned().collect::<std::collections::BTreeSet<_>>(),
                expected,
            );
            prop_assert!(!plan.trigrams_capped);
        }
    }
}
