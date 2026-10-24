//! Pure planning and source-coordinate handling for raw source prefixes.
use crate::evidence::SourceCoordinate;
use std::collections::BTreeSet;

pub const SOURCE_CONTEXT_POLICY: &str = "source-prefix-context/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySourceHit {
    pub episode: String,
    pub start: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceContextPlan {
    /// Episodes eligible for a prefix, in first-ranked-hit order.
    pub episodes: Vec<String>,
    pub unique_episodes: usize,
    pub eligible_episodes: usize,
    pub capped: bool,
}

/// Plan at most one prefix per episode, using only its first ranked query hit.
/// A first hit from byte zero already carries the source beginning and needs no
/// companion prefix, even if another hit for the episode starts later.
pub fn plan_prefixes(hits: &[QuerySourceHit], maximum: usize) -> SourceContextPlan {
    let mut seen = BTreeSet::new();
    let mut episodes = Vec::new();
    let mut unique_episodes = 0;
    let mut eligible_episodes = 0;
    let mut capped = false;

    for hit in hits {
        if !seen.insert(hit.episode.as_str()) {
            continue;
        }
        unique_episodes += 1;
        if hit.start == 0 {
            continue;
        }
        eligible_episodes += 1;
        if episodes.len() < maximum {
            episodes.push(hit.episode.clone());
        } else {
            capped = true;
        }
    }

    SourceContextPlan {
        episodes,
        unique_episodes,
        eligible_episodes,
        capped,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePrefix {
    pub text: String,
    pub source: SourceCoordinate,
}

/// Return the longest valid UTF-8 prefix within the byte limit, with exact
/// coordinates for the bytes actually returned. Invalid source bytes fail
/// closed; an incomplete scalar at the bounded edge is simply omitted.
pub fn utf8_prefix(
    bytes: &[u8],
    maximum_bytes: usize,
    source: &SourceCoordinate,
) -> Option<SourcePrefix> {
    let bounded = bytes.len().min(maximum_bytes);
    if bounded == 0 {
        return None;
    }
    let prefix = &bytes[..bounded];
    let end = match std::str::from_utf8(prefix) {
        Ok(_) => bounded,
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return None,
    };
    if end == 0 {
        return None;
    }
    let text = std::str::from_utf8(&bytes[..end]).ok()?.to_owned();
    let mut source = source.clone();
    source.start = 0;
    source.end = end;
    Some(SourcePrefix { text, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn coordinate(start: usize, end: usize) -> SourceCoordinate {
        SourceCoordinate {
            reference: "episode://source".into(),
            revision: "full-revision".into(),
            view: "episode-utf8/1".into(),
            view_digest: Some("full-view-digest".into()),
            decoder: Some("decoder/1".into()),
            start,
            end,
            document_uuid: Some("document-uuid".into()),
        }
    }

    #[test]
    fn first_ranked_hit_controls_prefix_eligibility_once_per_episode() {
        let hits = vec![
            QuerySourceHit {
                episode: "late-first".into(),
                start: 200,
            },
            QuerySourceHit {
                episode: "late-first".into(),
                start: 0,
            },
            QuerySourceHit {
                episode: "already-starts-at-zero".into(),
                start: 0,
            },
            QuerySourceHit {
                episode: "already-starts-at-zero".into(),
                start: 512,
            },
            QuerySourceHit {
                episode: "late-second".into(),
                start: 768,
            },
        ];

        let plan = plan_prefixes(&hits, 1);
        assert_eq!(plan.episodes, vec!["late-first"]);
        assert_eq!(plan.unique_episodes, 3);
        assert_eq!(plan.eligible_episodes, 2);
        assert!(plan.capped);
    }

    #[test]
    fn utf8_prefix_fails_closed_on_invalid_source_and_incomplete_only_scalar() {
        assert!(utf8_prefix(&[0xff, b'x'], 2, &coordinate(0, 2)).is_none());
        assert!(utf8_prefix("✓".as_bytes(), 1, &coordinate(0, 3)).is_none());
    }

    proptest! {
        #[test]
        fn prefix_plan_is_ordered_once_per_episode_and_bounded(
            raw_hits in prop::collection::vec((0u8..8, 0usize..4096), 0..80),
            maximum in 0usize..12,
        ) {
            let hits: Vec<_> = raw_hits.into_iter().map(|(episode, start)| QuerySourceHit {
                episode: format!("episode-{episode}"),
                start,
            }).collect();
            let plan = plan_prefixes(&hits, maximum);
            let mut first = std::collections::BTreeMap::new();
            for hit in &hits {
                first.entry(hit.episode.as_str()).or_insert(hit.start);
            }
            let expected: Vec<_> = first.iter().filter(|(_, start)| **start > 0)
                .map(|(episode, _)| *episode).collect();
            let actual: Vec<_> = plan.episodes.iter().map(String::as_str).collect();

            prop_assert_eq!(plan.unique_episodes, first.len());
            prop_assert_eq!(plan.eligible_episodes, expected.len());
            prop_assert_eq!(actual.len(), expected.len().min(maximum));
            prop_assert!(plan.episodes.len() <= maximum);
            prop_assert_eq!(plan.capped, expected.len() > maximum);
            // Stable output follows first-hit order, not the map's key order.
            let expected_ordered: Vec<_> = {
                let mut seen = BTreeSet::new();
                hits.iter().filter(|hit| seen.insert(hit.episode.as_str()) && hit.start > 0)
                    .map(|hit| hit.episode.as_str()).take(maximum).collect()
            };
            prop_assert_eq!(actual, expected_ordered);
        }

        #[test]
        fn utf8_prefix_has_exact_bounded_coordinates(
            text in ".{0,120}",
            maximum in 0usize..256,
        ) {
            let original = coordinate(30, 30 + text.len());
            if let Some(prefix) = utf8_prefix(text.as_bytes(), maximum, &original) {
                prop_assert!(!prefix.text.is_empty());
                prop_assert!(prefix.text.len() <= maximum);
                prop_assert!(text.starts_with(&prefix.text));
                prop_assert_eq!(prefix.source.start, 0);
                prop_assert_eq!(prefix.source.end, prefix.text.len());
                prop_assert_eq!(prefix.source.reference, original.reference);
                prop_assert_eq!(prefix.source.revision, original.revision);
                prop_assert_eq!(prefix.source.view, original.view);
                prop_assert_eq!(prefix.source.view_digest, original.view_digest);
                prop_assert_eq!(prefix.source.decoder, original.decoder);
                prop_assert_eq!(prefix.source.document_uuid, original.document_uuid);
            }
        }
    }
}
