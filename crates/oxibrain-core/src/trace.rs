//! Trajectory rendering contract (experience memory spec §2.2, M11).
//!
//! The caller renders an agent trajectory into episode text; oxibrain never
//! parses tool-call schemas (no agent framework is privileged — the P11
//! discipline applied to frameworks). This module is the **reference
//! implementation** of the documented contract, so agents (and tests) share
//! one canonical shape:
//!
//! ```text
//! [step 001 · 2026-10-16T02:11:04Z · tool: bash]
//! action: <verbatim>
//! observation: <verbatim, deterministically truncated>
//! ```
//!
//! Steps are separated by one blank line, which is the recursive chunk
//! ladder's first separator — so chunks approximate raw evidence slices
//! with no new index. Truncation is byte-counted, deterministic, and
//! char-boundary safe.

/// Rendering parameters. The observation cap is policy data, not code:
/// callers may lower it for verbose tools; the default matches the spec
/// (4,096 bytes per step).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TraceRenderPolicy {
    pub observation_cap: usize,
}

impl Default for TraceRenderPolicy {
    fn default() -> Self {
        Self {
            observation_cap: 4_096,
        }
    }
}

/// One rendered trajectory step. `timestamp` is RFC-3339 UTC; `None` falls
/// back to the run's `occurred_at` at render time.
#[derive(Debug, Clone)]
pub struct TraceStep<'a> {
    /// 1-based step ordinal; rendered zero-padded to three digits.
    pub ordinal: u32,
    pub timestamp: Option<&'a str>,
    /// Free-form tool token — no enum, no framework vocabulary (P11).
    pub tool: &'a str,
    pub action: &'a str,
    pub observation: &'a str,
}

/// Deterministically truncate an observation to `cap` bytes: first half +
/// an elision marker naming the byte count + last half. Multibyte-safe:
/// cut points retreat/advance to UTF-8 char boundaries, and the reported
/// elided count is computed after adjustment, so the output is a pure
/// function of the input.
pub fn truncate_observation(observation: &str, cap: usize) -> String {
    if observation.len() <= cap || cap < 8 {
        return observation.to_string();
    }
    let half = cap / 2;
    let mut head_end = half;
    while head_end > 0 && !observation.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = observation.len().saturating_sub(half);
    while tail_start < observation.len() && !observation.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if head_end >= tail_start {
        return observation.to_string();
    }
    let elided = tail_start - head_end;
    format!(
        "{}\n…[truncated {elided} bytes]…\n{}",
        &observation[..head_end],
        &observation[tail_start..]
    )
}

/// Render steps into episode text per the contract: blank-line separated
/// blocks, each `[step NNN · <ts> · tool: X]` + `action:` + `observation:`.
/// Scrubbing secrets before this call is the caller's duty; the in-brain
/// removal path is the audited `redact()`.
pub fn render_trajectory(
    steps: &[TraceStep<'_>],
    fallback_ts: &str,
    policy: &TraceRenderPolicy,
) -> String {
    steps
        .iter()
        .map(|s| {
            let ts = s.timestamp.unwrap_or(fallback_ts);
            format!(
                "[step {:03} · {} · tool: {}]\naction: {}\nobservation: {}",
                s.ordinal,
                ts,
                s.tool,
                s.action,
                truncate_observation(s.observation, policy.observation_cap)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(ordinal: u32, observation: &str) -> TraceStep<'_> {
        TraceStep {
            ordinal,
            timestamp: Some("2026-10-16T02:11:04Z"),
            tool: "bash",
            action: "cargo build --release",
            observation,
        }
    }

    #[test]
    fn renders_contract_shape() {
        let out = render_trajectory(
            &[step(1, "ok"), step(2, "also ok")],
            "2026-10-16T02:30:00Z",
            &TraceRenderPolicy::default(),
        );
        assert!(
            out.starts_with("[step 001 · 2026-10-16T02:11:04Z · tool: bash]\naction: cargo build --release\nobservation: ok"),
            "unexpected head: {out}"
        );
        assert!(
            out.contains("\n\n[step 002 · "),
            "steps not blank-line separated: {out}"
        );
        assert_eq!(
            out.matches("\n\n").count(),
            1,
            "exactly one separator between two steps"
        );
    }

    #[test]
    fn fallback_timestamp_used_when_missing() {
        let s = TraceStep {
            ordinal: 7,
            timestamp: None,
            tool: "edit",
            action: "fix",
            observation: "done",
        };
        let out = render_trajectory(&[s], "2026-10-16T09:00:00Z", &TraceRenderPolicy::default());
        assert!(
            out.contains("· 2026-10-16T09:00:00Z ·"),
            "fallback ts missing: {out}"
        );
        assert!(
            out.contains("[step 007 ·"),
            "ordinal not zero-padded: {out}"
        );
    }

    #[test]
    fn chunks_break_at_step_boundaries() {
        // Six steps, each well under the 4 KiB chunk max but together far
        // over it: the ladder's first separator (\n\n) must keep every
        // chunk a whole step — the raw-slice pool of the experience spec.
        let noise: String = "v".repeat(1_200);
        let observations: Vec<String> = (1..=6)
            .map(|i| format!("observation {i}: {noise}"))
            .collect();
        let steps: Vec<TraceStep<'_>> = observations
            .iter()
            .enumerate()
            .map(|(idx, obs)| step(idx as u32 + 1, obs))
            .collect();
        let content = render_trajectory(
            &steps,
            "2026-10-16T02:30:00Z",
            &TraceRenderPolicy::default(),
        );
        let chunks =
            crate::chunking::split_into_chunks(&content, &crate::chunking::ChunkPolicy::default());
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        for c in &chunks {
            let text = &content[c.span_start..c.span_end];
            let trimmed = text.trim_start_matches('\n');
            assert!(
                trimmed.starts_with("[step "),
                "chunk does not start at a step boundary: {:?}",
                &trimmed[..trimmed.len().min(60)]
            );
        }
    }

    #[test]
    fn truncation_deterministic_bounded_and_counted() {
        // Multibyte mix so char-boundary adjustment is exercised.
        let mut obs = String::new();
        while obs.len() < 10_000 {
            obs.push_str("프록시 뒤에서 npm 레지스트리에 접근할 수 없어 실패한다. ");
        }
        let a = truncate_observation(&obs, 4_096);
        let b = truncate_observation(&obs, 4_096);
        assert_eq!(a, b, "truncation must be a pure function");
        assert!(a.contains("…[truncated "), "elision marker missing");
        let elided: usize = a
            .split("…[truncated ")
            .nth(1)
            .and_then(|s| s.split(" bytes]…").next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(usize::MAX);
        // Simpler, robust accounting: head + tail + marker == output length.
        let marker_len = format!("\n…[truncated {elided} bytes]…\n").len();
        assert_eq!(
            obs.len() - elided + marker_len,
            a.len(),
            "elided+kept+marker must equal input"
        );
        assert!(a.len() < obs.len(), "truncation must shrink");
        assert!(
            a.starts_with(obs.chars().take(50).collect::<String>().trim()),
            "head preserved"
        );
    }

    #[test]
    fn short_observation_untouched() {
        assert_eq!(truncate_observation("small", 4_096), "small");
    }
}
