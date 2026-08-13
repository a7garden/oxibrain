//! Pipeline stage machine (DESIGN §9.1, F21, M10 10.10).
//!
//! The extraction pipeline is a sequence of stages: Ingest → Extract →
//! Validate → Project → Index. Each stage produces an `Outcome`; the pure
//! `step` function maps `(Stage, Outcome)` to the next stage (or `None` to
//! stop). Crash recovery is table-driven over this function — no database,
//! no model, no I/O.
//!
//! The facade calls `step` to decide what to do next after each stage
//! completes. The actual I/O (LLM calls, DB writes) happens in the facade;
//! `step` is the pure decision.

use serde::{Deserialize, Serialize};

/// Pipeline stages for the extraction pipeline (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Read episode text, insert into ledger + FTS.
    Ingest,
    /// Call the LLM extractor, parse the response.
    Extract,
    /// Validate claims against the registry + content.
    Validate,
    /// Project valid claims into entities, statements, assertions.
    Project,
    /// Update FTS + embedding index for affected entities.
    Index,
}

/// Outcome of a stage execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Stage completed successfully.
    Ok,
    /// Stage failed initially but recovered (e.g. repair loop succeeded).
    Recovered,
    /// Stage failed — pipeline stops. The error message is recorded.
    Failed(String),
    /// Stage was skipped (e.g. no pending jobs, cache hit).
    Skipped,
}

/// Pure state transition: given the current stage and its outcome, what's next?
///
/// - `Ok` / `Recovered` → advance to the next stage.
/// - `Failed` / `Skipped` → stop (return `None`).
/// - After `Index` → pipeline complete (return `None`).
///
/// This function is the single decision point for crash recovery. All
/// table-driven crash tests run against it — no database, no model.
pub fn step(stage: Stage, outcome: &Outcome) -> Option<Stage> {
    match outcome {
        Outcome::Failed(_) | Outcome::Skipped => None,
        Outcome::Ok | Outcome::Recovered => match stage {
            Stage::Ingest => Some(Stage::Extract),
            Stage::Extract => Some(Stage::Validate),
            Stage::Validate => Some(Stage::Project),
            Stage::Project => Some(Stage::Index),
            Stage::Index => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Table-driven crash tests (§9.1, exit criterion) ─────────────────
    // Each case: (stage, outcome) → expected next stage (or None to stop).
    // No database, no model — pure decision.

    #[test]
    fn happy_path_advances_through_all_stages() {
        let mut stage = Stage::Ingest;
        let path = [
            Stage::Extract,
            Stage::Validate,
            Stage::Project,
            Stage::Index,
        ];
        for expected in path {
            assert_eq!(step(stage, &Outcome::Ok), Some(expected));
            stage = expected;
        }
        // After Index → pipeline complete.
        assert_eq!(step(Stage::Index, &Outcome::Ok), None);
    }

    #[test]
    fn recovered_advances_like_ok() {
        assert_eq!(
            step(Stage::Extract, &Outcome::Recovered),
            Some(Stage::Validate)
        );
    }

    #[test]
    fn failure_stops_pipeline_from_any_stage() {
        for stage in [
            Stage::Ingest,
            Stage::Extract,
            Stage::Validate,
            Stage::Project,
            Stage::Index,
        ] {
            assert_eq!(
                step(stage, &Outcome::Failed("timeout".into())),
                None,
                "pipeline should stop on failure from {stage:?}"
            );
        }
    }

    #[test]
    fn skipped_stops_pipeline_from_any_stage() {
        for stage in [
            Stage::Ingest,
            Stage::Extract,
            Stage::Validate,
            Stage::Project,
            Stage::Index,
        ] {
            assert_eq!(
                step(stage, &Outcome::Skipped),
                None,
                "pipeline should stop on skip from {stage:?}"
            );
        }
    }

    #[test]
    fn stage_order_is_linear_no_shortcuts() {
        // No stage should skip ahead — each must pass through the next.
        assert_eq!(step(Stage::Ingest, &Outcome::Ok), Some(Stage::Extract));
        assert_eq!(step(Stage::Extract, &Outcome::Ok), Some(Stage::Validate));
        assert_eq!(step(Stage::Validate, &Outcome::Ok), Some(Stage::Project));
        assert_eq!(step(Stage::Project, &Outcome::Ok), Some(Stage::Index));
    }
}
