//! Confidence calibration (DESIGN §6.5).
//!
//! confidence = calibrate(extractor) · corroboration · trust · recency_of_support
//!
//! The calibration multiplier is per-extractor, measured by the eval harness.
//! An unmeasured extractor gets a conservative prior of 0.8.

use serde::{Deserialize, Serialize};

/// Components of the confidence formula (DESIGN §6.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceComponents {
    /// Raw confidence from the LLM assertion.
    pub raw: f32,
    /// Per-extractor calibration multiplier [0.1, 2.0].
    pub calibrated: f32,
    /// Saturating in distinct supporting episodes [0.5, 1.0].
    pub corroboration: f32,
    /// Weighted by episode trust tier [0.3, 1.0].
    pub trust: f32,
    /// Recency_of_support for Interval predicates [0.5, 1.0].
    pub recency: f32,
}

impl ConfidenceComponents {
    /// Compute final confidence. Clamped to [0.0, 1.0].
    pub fn combine(&self) -> f32 {
        let c = self.raw * self.calibrated * self.corroboration * self.trust * self.recency;
        c.clamp(0.0, 1.0)
    }
}

/// Stores per-extractor calibration values (loaded from eval results).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CalibrationTable {
    pub values: std::collections::BTreeMap<String, f32>,
}

impl CalibrationTable {
    /// Look up the calibration multiplier for an extractor.
    /// Returns `None` for unmeasured extractors; the caller applies the prior.
    pub fn get(&self, extractor_id: &str) -> Option<f32> {
        self.values.get(extractor_id).copied()
    }

    /// Set the calibration multiplier for an extractor. Clamped to [0.1, 2.0].
    pub fn set(&mut self, extractor_id: &str, value: f32) {
        self.values
            .insert(extractor_id.to_string(), value.clamp(0.1, 2.0));
    }

    /// Serialize to JSON for storage in the `meta` table.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("calibration table serializable")
    }

    /// Deserialize from JSON (meta table).
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// Per-extractor calibration multiplier. An unmeasured extractor gets a
/// conservative prior of 0.8.
pub fn calibrate(extractor_id: &str, table: &CalibrationTable) -> f32 {
    table.get(extractor_id).unwrap_or(0.8)
}

/// Derive a calibration multiplier from eval metrics.
/// Higher precision → higher multiplier (trust the extractor more).
/// Fabricated entities → penalty.
pub fn derive_calibration(precision: f64, fabrication_rate: f64) -> f32 {
    let base = 0.8_f32;
    let precision_factor = precision as f32;
    let fabrication_penalty = 1.0 - fabrication_rate as f32;
    (base * precision_factor * fabrication_penalty).clamp(0.1, 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_clamps_to_unit_range() {
        let c = ConfidenceComponents {
            raw: 2.0,
            calibrated: 2.0,
            corroboration: 2.0,
            trust: 2.0,
            recency: 2.0,
        };
        assert_eq!(c.combine(), 1.0);

        let c = ConfidenceComponents {
            raw: 0.0,
            calibrated: 0.5,
            corroboration: 0.5,
            trust: 0.5,
            recency: 0.5,
        };
        assert_eq!(c.combine(), 0.0);
    }

    #[test]
    fn calibrate_returns_prior_for_unknown() {
        let table = CalibrationTable::default();
        assert_eq!(calibrate("unknown", &table), 0.8);
    }

    #[test]
    fn calibrate_returns_stored_value() {
        let mut table = CalibrationTable::default();
        table.set("ext1", 1.2);
        assert_eq!(calibrate("ext1", &table), 1.2);
    }

    #[test]
    fn calibration_table_roundtrip() {
        let mut table = CalibrationTable::default();
        table.set("ext1", 1.0);
        table.set("ext2", 0.5);
        let json = table.to_json();
        let restored = CalibrationTable::from_json(&json);
        assert_eq!(restored.get("ext1"), Some(1.0));
        assert_eq!(restored.get("ext2"), Some(0.5));
    }

    #[test]
    fn derive_calibration_monotonic_in_precision() {
        let low = derive_calibration(0.5, 0.0);
        let high = derive_calibration(0.95, 0.0);
        assert!(high > low);
    }

    #[test]
    fn derive_calibration_penalizes_fabrication() {
        let clean = derive_calibration(0.9, 0.0);
        let dirty = derive_calibration(0.9, 0.1);
        assert!(clean > dirty);
    }
}
