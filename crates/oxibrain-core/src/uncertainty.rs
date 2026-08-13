//! Uncertainty quantification for derived artifacts (DESIGN §13.1, P10, D23).
//!
//! P10: "Compression may lose detail, never doubt." Every derived episode
//! (consolidation, community summary) carries an `Uncertainty` computed from
//! its support — contradictions, single-source claims, staleness, and trust
//! exclusions. A summary is never returned without its sources.
//!
//! The `compute` function is pure: same inputs → same `Uncertainty`. It takes
//! belief statistics (counts) as input, not raw database rows, so it can be
//! property-tested without a database.

use serde::{Deserialize, Serialize};

/// The four uncertainty factors computed from a group's support (§13.1).
/// Each is in [0.0, 1.0]; 0.0 means "no uncertainty from this factor."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Uncertainty {
    /// Fraction of beliefs in the group that are contradicted.
    pub contradiction_rate: f32,
    /// Fraction of beliefs backed by only one episode.
    pub single_source_fraction: f32,
    /// Normalised age of the oldest supporting episode (0 = fresh, 1 = ≥ 1 year).
    pub staleness: f32,
    /// Fraction of beliefs with untrusted support.
    pub trust_exclusion_fraction: f32,
}

impl Uncertainty {
    /// Aggregate uncertainty score — a single number in [0, 1] representing
    /// overall doubt. Weighted sum: contradictions dominate, then single
    /// source, then staleness, then trust exclusions.
    pub fn score(&self) -> f32 {
        (self.contradiction_rate * 0.4
            + self.single_source_fraction * 0.25
            + self.staleness * 0.15
            + self.trust_exclusion_fraction * 0.2)
            .clamp(0.0, 1.0)
    }
}

impl Default for Uncertainty {
    fn default() -> Self {
        Self {
            contradiction_rate: 0.0,
            single_source_fraction: 0.0,
            staleness: 0.0,
            trust_exclusion_fraction: 0.0,
        }
    }
}

/// Raw belief statistics from which `Uncertainty` is computed. The store
/// gathers these from a single GROUP BY query over the group's beliefs.
#[derive(Debug, Clone, Default)]
pub struct UncertaintyInput {
    /// Total beliefs in the group.
    pub total_beliefs: usize,
    /// Beliefs with status = contradicted.
    pub contradicted_beliefs: usize,
    /// Beliefs backed by only 1 distinct episode.
    pub single_source_beliefs: usize,
    /// Beliefs whose support includes an untrusted episode.
    pub untrusted_beliefs: usize,
    /// Age of the oldest supporting episode in days.
    pub max_episode_age_days: f64,
}

/// Compute uncertainty from belief statistics (§13.1, P10). Pure function.
///
/// `max_episode_age_days` is normalised to staleness: 0 days → 0.0,
/// 365 days → 1.0, clamped.
pub fn compute(input: &UncertaintyInput) -> Uncertainty {
    let total = input.total_beliefs.max(1) as f32;
    let staleness = (input.max_episode_age_days / 365.0).clamp(0.0, 1.0) as f32;
    Uncertainty {
        contradiction_rate: (input.contradicted_beliefs as f32 / total).clamp(0.0, 1.0),
        single_source_fraction: (input.single_source_beliefs as f32 / total).clamp(0.0, 1.0),
        staleness,
        trust_exclusion_fraction: (input.untrusted_beliefs as f32 / total).clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_group_is_zero_uncertainty() {
        let u = compute(&UncertaintyInput::default());
        // total_beliefs = 0 → total = max(0, 1) = 1 → all fractions are 0.
        assert_eq!(u, Uncertainty::default());
        assert_eq!(u.score(), 0.0);
    }

    #[test]
    fn all_contradicted_gives_max_contradiction_rate() {
        let input = UncertaintyInput {
            total_beliefs: 10,
            contradicted_beliefs: 10,
            ..Default::default()
        };
        let u = compute(&input);
        assert!((u.contradiction_rate - 1.0).abs() < f32::EPSILON);
        assert!(u.score() > 0.35); // 0.4 * 1.0 dominates
    }

    #[test]
    fn score_weights_contradictions_above_single_source() {
        let contradictions_only = compute(&UncertaintyInput {
            total_beliefs: 10,
            contradicted_beliefs: 5,
            ..Default::default()
        });
        let single_source_only = compute(&UncertaintyInput {
            total_beliefs: 10,
            single_source_beliefs: 5,
            ..Default::default()
        });
        assert!(contradictions_only.score() > single_source_only.score());
    }

    #[test]
    fn staleness_normalises_to_year() {
        let fresh = compute(&UncertaintyInput {
            max_episode_age_days: 0.0,
            ..Default::default()
        });
        let half_year = compute(&UncertaintyInput {
            max_episode_age_days: 182.0,
            ..Default::default()
        });
        let one_year = compute(&UncertaintyInput {
            max_episode_age_days: 365.0,
            ..Default::default()
        });
        let two_years = compute(&UncertaintyInput {
            max_episode_age_days: 730.0,
            ..Default::default()
        });
        assert!(fresh.staleness < half_year.staleness);
        assert!((one_year.staleness - 1.0).abs() < 0.01);
        assert!((two_years.staleness - 1.0).abs() < f32::EPSILON); // clamped
    }

    #[test]
    fn compute_is_pure() {
        let input = UncertaintyInput {
            total_beliefs: 8,
            contradicted_beliefs: 2,
            single_source_beliefs: 3,
            untrusted_beliefs: 1,
            max_episode_age_days: 100.0,
        };
        let u1 = compute(&input);
        let u2 = compute(&input);
        assert_eq!(u1, u2);
    }

    #[test]
    fn score_is_in_unit_interval() {
        for cr in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            for ss in [0.0f32, 0.5, 1.0] {
                for st in [0.0f32, 0.5, 1.0] {
                    for te in [0.0f32, 0.5, 1.0] {
                        let u = Uncertainty {
                            contradiction_rate: cr,
                            single_source_fraction: ss,
                            staleness: st,
                            trust_exclusion_fraction: te,
                        };
                        let s = u.score();
                        assert!(
                            (0.0..=1.0).contains(&s),
                            "score {s} out of range for cr={cr} ss={ss} st={st} te={te}"
                        );
                    }
                }
            }
        }
    }
}
