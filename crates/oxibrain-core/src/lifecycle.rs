//! Lifecycle types: salience decay, compaction config (DESIGN §10).

use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayConfig {
    pub base: f64,
    pub lambda: f64,
    pub floor: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            base: 1.0,
            lambda: 0.01,
            floor: 0.05,
        }
    }
}

/// Pure salience computation. Deterministic from the ledger.
pub fn salience(last_activity: Timestamp, now: Timestamp, config: &DecayConfig) -> f64 {
    let age_millis = (now.millis() - last_activity.millis()).max(0) as f64;
    let age_days = age_millis / 86_400_000.0;
    let decayed = config.base * (-config.lambda * age_days).exp();
    decayed.max(config.floor)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub salience_threshold: f64,
    pub min_age_days: u32,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            salience_threshold: 0.1,
            min_age_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalienceEntry {
    pub entity_id: String,
    pub salience: f64,
    pub last_activity: Timestamp,
}
