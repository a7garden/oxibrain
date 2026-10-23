//! Gold source-span evaluation. String presence is never sufficient support.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportSpan {
    pub source: String,
    pub start: usize,
    pub end: usize,
}

/// Fraction of required source bytes recovered by a union of selected ranges.
pub fn support_fraction(gold: &[SupportSpan], selected: &[SupportSpan]) -> f64 {
    let mut required = 0;
    let mut recovered = 0;
    for g in gold {
        if g.end <= g.start {
            continue;
        }
        required += g.end - g.start;
        let mut ranges: Vec<_> = selected
            .iter()
            .filter(|s| s.source == g.source)
            .map(|s| (s.start.max(g.start), s.end.min(g.end)))
            .filter(|(a, b)| a < b)
            .collect();
        ranges.sort_unstable();
        let mut end = g.start;
        for (a, b) in ranges {
            if b > end {
                recovered += b - a.max(end);
                end = b;
            }
        }
    }
    if required == 0 {
        0.0
    } else {
        recovered as f64 / required as f64
    }
}

/// Paired bootstrap by source trajectory, not by correlated question rows.
/// Deterministic seed and 2,000 resamples make an offline replay reproducible.
pub fn paired_interval(groups: &[(String, f64)]) -> [f64; 2] {
    let mut by_group = std::collections::BTreeMap::<String, Vec<f64>>::new();
    for (key, difference) in groups {
        by_group.entry(key.clone()).or_default().push(*difference);
    }
    let rows: Vec<_> = by_group.values().collect();
    if rows.is_empty() {
        return [0.0, 0.0];
    }
    let mut state = 0x4f5849425241494eu64;
    let mut samples = Vec::new();
    for _ in 0..2000 {
        let mut sum = 0.0;
        let mut n = 0;
        for _ in 0..rows.len() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let row = rows[(state as usize) % rows.len()];
            sum += row.iter().sum::<f64>();
            n += row.len();
        }
        samples.push(sum / n as f64);
    }
    samples.sort_by(f64::total_cmp);
    [samples[50], samples[1949]]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrong_relation_or_source_does_not_get_credit() {
        let gold = [SupportSpan {
            source: "production".into(),
            start: 50,
            end: 100,
        }];
        assert_eq!(
            support_fraction(
                &gold,
                &[SupportSpan {
                    source: "staging".into(),
                    start: 50,
                    end: 100
                }]
            ),
            0.0
        );
        assert_eq!(
            support_fraction(
                &gold,
                &[SupportSpan {
                    source: "production".into(),
                    start: 0,
                    end: 50
                }]
            ),
            0.0
        );
        assert_eq!(
            support_fraction(
                &gold,
                &[
                    SupportSpan {
                        source: "production".into(),
                        start: 50,
                        end: 80
                    },
                    SupportSpan {
                        source: "production".into(),
                        start: 70,
                        end: 100
                    }
                ]
            ),
            1.0
        );
    }
    #[test]
    fn grouped_interval_is_deterministic() {
        let g = vec![("a".into(), 1.0), ("a".into(), 0.0), ("b".into(), -1.0)];
        assert_eq!(paired_interval(&g), paired_interval(&g));
    }
}
