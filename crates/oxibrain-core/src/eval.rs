//! Evaluation metrics (DESIGN §14.2). Pure functions comparing extracted
//! assertions against golden-corpus annotations.

use serde::{Deserialize, Serialize};

/// Quality metrics computed from an extraction run against golden annotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalMetrics {
    /// Entities extracted that don't appear in the source text. Structural hard zero.
    pub fabricated_entity_rate: f64,
    /// Correct statements / total extracted statements.
    pub statement_precision: f64,
    /// Correct statements / total expected statements.
    pub statement_recall: f64,
    /// Harmonic mean of precision and recall (for statements).
    pub statement_f1: f64,
}

/// A triple extracted from the brain, for comparison against golden annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedTriple {
    pub predicate: String,
    pub subject_surface: String,
    pub object_surface: String,
}

/// Compute metrics by comparing extracted triples against expected triples.
///
/// A triple is "correct" if it matches an expected triple by
/// (predicate, subject_surface, object_surface) — case-insensitive on surfaces.
pub fn compute_metrics(extracted: &[ExtractedTriple], expected: &[ExtractedTriple]) -> EvalMetrics {
    let extracted_set: std::collections::HashSet<(String, String, String)> = extracted
        .iter()
        .map(|t| {
            (
                t.predicate.to_lowercase(),
                t.subject_surface.to_lowercase(),
                t.object_surface.to_lowercase(),
            )
        })
        .collect();

    let expected_set: std::collections::HashSet<(String, String, String)> = expected
        .iter()
        .map(|t| {
            (
                t.predicate.to_lowercase(),
                t.subject_surface.to_lowercase(),
                t.object_surface.to_lowercase(),
            )
        })
        .collect();

    let correct = extracted_set.intersection(&expected_set).count();
    let extracted_count = extracted_set.len();
    let expected_count = expected_set.len();

    let precision = if extracted_count > 0 {
        correct as f64 / extracted_count as f64
    } else {
        0.0
    };
    let recall = if expected_count > 0 {
        correct as f64 / expected_count as f64
    } else {
        1.0 // vacuously true if no expectations
    };
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };

    // Fabricated entity rate: 0.0 by construction (validator enforces verbatim surfaces).
    // This is a structural guarantee, not a measured quantity.
    let fabricated_entity_rate = 0.0;

    EvalMetrics {
        fabricated_entity_rate,
        statement_precision: precision,
        statement_recall: recall,
        statement_f1: f1,
    }
}

/// Measure fabricated entity rate (§17.3, F19, 10.7): of the entity surfaces
/// extracted, what fraction do NOT appear verbatim in the source text?
/// This detects hallucinated entities that slipped past validation.
///
/// Pure function: same inputs → same rate. Returns 0.0 for empty input.
pub fn measure_fabrication_rate(entity_surfaces: &[String], source_text: &str) -> f64 {
    if entity_surfaces.is_empty() {
        return 0.0;
    }
    let fabricated = entity_surfaces
        .iter()
        .filter(|surface| !source_text.contains(surface.as_str()))
        .count();
    fabricated as f64 / entity_surfaces.len() as f64
}

/// Like `compute_metrics` but with a measured fabrication rate (§17.3, 10.7).
/// The fabricated rate is computed separately by `measure_fabrication_rate`
/// because it requires the source text, which `compute_metrics` does not take.
pub fn compute_metrics_with_fabrication(
    extracted: &[ExtractedTriple],
    expected: &[ExtractedTriple],
    fabricated_entity_rate: f64,
) -> EvalMetrics {
    let mut metrics = compute_metrics(extracted, expected);
    metrics.fabricated_entity_rate = fabricated_entity_rate;
    metrics
}

impl EvalMetrics {
    /// Check §14.2 quality gates. Returns Err with details if any gate fails.
    pub fn check_gates(&self) -> Result<(), String> {
        if self.fabricated_entity_rate != 0.0 {
            return Err(format!(
                "fabricated_entity_rate = {:.4}, expected 0.00 (structural hard zero)",
                self.fabricated_entity_rate
            ));
        }
        if self.statement_precision < 0.90 {
            return Err(format!(
                "statement_precision = {:.4}, expected ≥ 0.90",
                self.statement_precision
            ));
        }
        if self.statement_recall < 0.70 {
            return Err(format!(
                "statement_recall = {:.4}, expected ≥ 0.70",
                self.statement_recall
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_extraction() {
        let triples = vec![
            triple("works_on", "Alice", "ProjectX"),
            triple("employed_by", "Alice", "Acme"),
        ];
        let m = compute_metrics(&triples, &triples);
        assert_eq!(m.statement_precision, 1.0);
        assert_eq!(m.statement_recall, 1.0);
        assert_eq!(m.statement_f1, 1.0);
        assert!(m.check_gates().is_ok());
    }

    #[test]
    fn partial_match() {
        let extracted = vec![
            triple("works_on", "Alice", "ProjectX"),
            triple("employed_by", "Alice", "Acme"),
        ];
        let expected = vec![
            triple("works_on", "Alice", "ProjectX"),
            triple("employed_by", "Alice", "Acme"),
            triple("knows", "Alice", "Bob"),
        ];
        let m = compute_metrics(&extracted, &expected);
        // precision = 2/2 = 1.0, recall = 2/3 ≈ 0.667
        assert_eq!(m.statement_precision, 1.0);
        assert!((m.statement_recall - 0.667).abs() < 0.01);
    }

    #[test]
    fn case_insensitive_matching() {
        let extracted = vec![triple("WORKS_ON", "alice", "projectx")];
        let expected = vec![triple("works_on", "Alice", "ProjectX")];
        let m = compute_metrics(&extracted, &expected);
        assert_eq!(m.statement_precision, 1.0);
        assert_eq!(m.statement_recall, 1.0);
    }

    #[test]
    fn empty_extraction_zero_precision() {
        let expected = vec![triple("works_on", "Alice", "ProjectX")];
        let m = compute_metrics(&[], &expected);
        assert_eq!(m.statement_precision, 0.0);
        assert_eq!(m.statement_recall, 0.0);
    }

    fn triple(p: &str, s: &str, o: &str) -> ExtractedTriple {
        ExtractedTriple {
            predicate: p.into(),
            subject_surface: s.into(),
            object_surface: o.into(),
        }
    }

    // ── Fabrication measurement (§17.3, 10.7) ──────────────────────────

    #[test]
    fn fabrication_rate_zero_when_all_surfaces_present() {
        let surfaces = vec!["Alice".to_string(), "Acme".to_string()];
        let text = "Alice works at Acme Corp";
        assert_eq!(measure_fabrication_rate(&surfaces, text), 0.0);
    }

    #[test]
    fn fabrication_rate_nonzero_when_surface_missing() {
        let surfaces = vec!["Alice".to_string(), "Hallucinated".to_string()];
        let text = "Alice works at Acme Corp";
        let rate = measure_fabrication_rate(&surfaces, text);
        assert!((rate - 0.5).abs() < 0.01, "1 of 2 fabricated, got {rate}");
    }

    #[test]
    fn fabrication_rate_empty_input_is_zero() {
        assert_eq!(measure_fabrication_rate(&[], "any text"), 0.0);
    }
}
