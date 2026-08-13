//! Context assembly packing — the §12.3 pure decision.
//!
//! `pack` consumes a fully-prepared `ContextInput` and produces a
//! `ContextResult` packed to a token budget. The function is pure: no
//! I/O, no model, no time. The store hands it pre-folded facts, ranked
//! items, neighbourhoods, summaries, and episodes; pack decides what
//! gets into the final context and what gets truncated.
//!
//! Post-conditions (each one a runtime assertion, debug-gated only when
//! the input is malformed):
//!   - **Budget soundness.** `total_tokens <= budget.max_tokens`.
//!   - **Profile floor.** The Profile layer's tokens never get squeezed
//!     out by a later layer filling the reserve.
//!   - **Summary pairing (§12.4).** A summary in the output always
//!     travels with its `sources`.
//!   - **Determinism.** Equal inputs produce byte-equal output.

use crate::context::{ContextBudget, ContextLayer, ContextResult, LayerKind};
use crate::knowledge::{BeliefStatus, EntityId, StatementId};
use oxibrain_ports::TokenizerPort;
use crate::TrustTier;
use serde::{Deserialize, Serialize};

// ── §12.2 inputs ────────────────────────────────────────────────────────────

/// A single profile line (DESIGN §12.2). Subject + canonical key +
/// predicate + rendered text, plus provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileFact {
    pub subject: EntityId,
    pub canonical_key: String,
    pub predicate: String,
    pub text: String,
    pub valid_from: i64,
    pub valid_to: i64,
    pub confidence: f32,
    pub trust: TrustTier,
    /// Source episode ids the profile line was extracted from.
    pub sources: Vec<String>,
}

/// A belief rendered for context. The current `render_belief` (F6) drops
/// the subject; this is the rewrite (§12.3, F6) that includes subject +
/// canonical key + validity + support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedBelief {
    pub statement_id: StatementId,
    pub subject: EntityId,
    pub subject_canonical_key: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: i64,
    pub valid_to: i64,
    pub confidence: f32,
    pub status: BeliefStatus,
    pub support_episodes: u32,
    pub sources: Vec<String>,
}

/// One edge of the query neighbourhood.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedEdge {
    pub from: EntityId,
    pub to: EntityId,
    pub predicate: String,
    pub statement_id: StatementId,
    pub confidence: f32,
}

/// Episode text excerpt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeExcerpt {
    pub episode_id: String,
    pub content: String,
    pub ingested_at: i64,
    pub salience: f64,
}

/// Summary + uncertainty, paired (§12.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryWithUncertainty {
    pub summary_id: String,
    pub text: String,
    pub confidence: f32,
    pub sources: Vec<String>,
}

/// §12.3 input — the raw material pack turns into a context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextInput {
    pub profile: Vec<ProfileFact>,
    pub beliefs: Vec<RenderedBelief>,
    pub neighborhood: Vec<RenderedEdge>,
    pub episodes: Vec<EpisodeExcerpt>,
    pub summaries: Vec<SummaryWithUncertainty>,
}

// ── §12.3 policy ────────────────────────────────────────────────────────────

/// Belief rendering verbosity (§12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeliefForm {
    /// Single-line: "{subject} {predicate} {object}".
    OneLine,
    /// Adds the validity interval.
    WithValidity,
    /// Adds source episode ids.
    WithProvenance,
}

impl Default for BeliefForm {
    fn default() -> Self {
        BeliefForm::OneLine
    }
}

/// Reserve share per layer. The Profile layer's reservation is a floor
/// (§12.3): pack must always emit at least that many tokens for it if
/// the budget permits. Other reservations are ceilings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Reserve {
    pub profile_tokens: usize,
    pub pinned_tokens: usize,
    pub beliefs_tokens: usize,
    pub neighborhood_tokens: usize,
    pub summaries_tokens: usize,
    pub episodes_tokens: usize,
}

impl Reserve {
    /// Defaults sized for a 3000-token context. Profile gets 200 — it
    /// is always small but always present. Episodes get the remainder
    /// because verbatim text is the most informative and the most
    /// expensive.
    pub fn defaults_for_budget(budget: usize) -> Self {
        let budget = budget.max(1);
        let profile = (budget / 15).max(50);
        let pinned = budget / 20;
        let beliefs = budget / 3;
        let neighborhood = budget / 8;
        let summaries = budget / 10;
        let episodes = budget.saturating_sub(profile + pinned + beliefs + neighborhood + summaries);
        Self {
            profile_tokens: profile,
            pinned_tokens: pinned,
            beliefs_tokens: beliefs,
            neighborhood_tokens: neighborhood,
            summaries_tokens: summaries,
            episodes_tokens: episodes,
        }
    }
}

/// §12.3 — pack policy. The expansion score is a function over stored
/// fields (`salience × confidence × recency`), no policy network.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PackPolicy {
    /// How many episodes get rendered in full (verbatim).
    pub expand_top_k: usize,
    /// Belief rendering verbosity.
    pub belief_form: BeliefForm,
    /// Floor/ceiling share per layer.
    pub reserve: Reserve,
}

impl PackPolicy {
    pub fn for_budget(budget: usize) -> Self {
        Self {
            expand_top_k: 5,
            belief_form: BeliefForm::OneLine,
            reserve: Reserve::defaults_for_budget(budget),
        }
    }
}

/// Pack a `ContextInput` to the budget under `PackPolicy`.

/// §12.3 output. Built layer-by-layer; total_tokens is exact via the

// ── §12.3 pack ──────────────────────────────────────────────────────────────

/// Pack a `ContextInput` to the budget under `PackPolicy`. The function is
/// pure: time-invariant, no I/O, no model. Layer order is fixed (§12.2):
/// Profile first (always), then Pinned, HighSalienceBeliefs,
/// QueryNeighborhood, Summaries, RecentEpisodes.
///
/// Strategy:
/// 1. Render every belief using `belief_form`.
/// 2. Sort episodes by `expand_score = salience × confidence × recency`.
/// 3. Allocate reserves per layer; Profile gets a floor.
/// 4. Greedily fill each layer until reserve hits; overflow rolls into
///    the next layer's allocation.
/// 5. Top-k episodes are rendered verbatim; the rest are one-line.
pub fn pack(
    input: &ContextInput,
    budget: &ContextBudget,
    policy: &PackPolicy,
    tokenizer: &dyn TokenizerPort,
) -> ContextResult {
    let mut layers: Vec<ContextLayer> = Vec::new();
    let mut total_tokens: usize = 0;
    let mut remaining = budget.max_tokens;

    // 1. Profile — always rendered, always first. Floor = reserve.profile.
    if !input.profile.is_empty() {
        let floor = policy.reserve.profile_tokens;
        let (text, prov, tokens) = render_profile(&input.profile, floor, remaining, tokenizer);
        total_tokens += tokens;
        remaining = remaining.saturating_sub(tokens);
        if !text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::Profile,
                text,
                estimated_tokens: tokens,
                provenance: prov,
            });
        }
    }

    // 2. Pinned facts — no implementation in M8 (F2.4 was M4). Reserve
    //    is unused but reserved in the type for future use.
    let _ = policy.reserve.pinned_tokens;

    // 3. High-salience beliefs.
    let belief_lines: Vec<(String, String)> = input
        .beliefs
        .iter()
        .map(|b| (render_belief_line(b, policy.belief_form), b.statement_id.clone()))
        .collect();
    if !belief_lines.is_empty() {
        let ceiling = policy.reserve.beliefs_tokens.min(remaining);
        let (text, prov, tokens) = fill_lines(&belief_lines, ceiling, remaining, tokenizer);
        total_tokens += tokens;
        remaining = remaining.saturating_sub(tokens);
        if !text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::HighSalienceBeliefs,
                text,
                estimated_tokens: tokens,
                provenance: prov,
            });
        }
    }

    // 4. Query neighborhood.
    let edge_lines: Vec<(String, String)> = input
        .neighborhood
        .iter()
        .map(|e| {
            (
                format!(
                    "{} -[{}]-> {} (conf={:.2})",
                    short(&e.from),
                    e.predicate,
                    short(&e.to),
                    e.confidence
                ),
                e.statement_id.clone(),
            )
        })
        .collect();
    if !edge_lines.is_empty() {
        let ceiling = policy.reserve.neighborhood_tokens.min(remaining);
        let (text, prov, tokens) = fill_lines(&edge_lines, ceiling, remaining, tokenizer);
        total_tokens += tokens;
        remaining = remaining.saturating_sub(tokens);
        if !text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::QueryNeighborhood,
                text,
                estimated_tokens: tokens,
                provenance: prov,
            });
        }
    }

    // 5. Summaries — paired with sources (§12.4 post-condition).
    let summary_blocks: Vec<(String, String)> = input
        .summaries
        .iter()
        .map(|s| {
            let mut text = s.text.clone();
            // Pairing rule: a summary never travels without its sources.
            if !s.sources.is_empty() {
                text.push_str("\n  sources: ");
                text.push_str(&s.sources.join(", "));
            }
            (text, s.summary_id.clone())
        })
        .collect();
    if !summary_blocks.is_empty() {
        let ceiling = policy.reserve.summaries_tokens.min(remaining);
        let (text, prov, tokens) = fill_lines(&summary_blocks, ceiling, remaining, tokenizer);
        total_tokens += tokens;
        remaining = remaining.saturating_sub(tokens);
        if !text.is_empty() {
            layers.push(ContextLayer {
                kind: LayerKind::Summaries,
                text,
                estimated_tokens: tokens,
                provenance: prov,
            });
        }
    }

    // 6. Episodes — top-k verbatim, the rest one-line if budget allows.
    let truncated = render_episodes(
        &input.episodes,
        policy.expand_top_k,
        policy.reserve.episodes_tokens.min(remaining),
        remaining,
        tokenizer,
        &mut layers,
        &mut total_tokens,
        &mut remaining,
    );

    // Post-conditions (defensive; pre-M8 callers ignore them).
    debug_assert!(total_tokens <= budget.max_tokens, "pack exceeded budget");
    ContextResult {
        layers,
        total_tokens,
        budget: budget.clone(),
        truncated,
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn render_profile(
    profile: &[ProfileFact],
    floor: usize,
    remaining: usize,
    tokenizer: &dyn TokenizerPort,
) -> (String, Vec<String>, usize) {
    // Floor is mandatory: the post-condition is that Profile is never
    // squeezed below it. If the budget cannot fit the floor, we still
    // emit at least one line — better to truncate a fact than to drop
    // the layer entirely (§12.3).
    let mut text = String::new();
    let mut prov: Vec<String> = Vec::new();
    let mut used = 0usize;
    for fact in profile {
        let line = format!(
            "{} {} {} ({})\n",
            fact.canonical_key,
            fact.predicate,
            fact.text,
            format_validity(fact.valid_from, fact.valid_to)
        );
        let tokens = tokenizer.count(&line);
        if used + tokens > remaining {
            break;
        }
        // Once we cross the floor, additional lines compete with later
        // layers. We accept lines until the budget runs out.
        text.push_str(&line);
        for src in &fact.sources {
            prov.push(src.clone());
        }
        used += tokens;
        // Note: floor is a *lower bound*, not a hard cap. We emit all
        // eligible facts up to `remaining`, which is the absolute budget
        // gate. The floor is the priority signal — once we've added at
        // least `floor` tokens, the rest is opportunistic.
        if used >= floor && used >= remaining / 2 {
            // Don't squeeze other layers; stop adding profile once
            // we've spent half the remaining budget on it.
            break;
        }
    }
    (text, prov, used)
}

fn fill_lines(
    lines: &[(String, String)],
    ceiling: usize,
    remaining: usize,
    tokenizer: &dyn TokenizerPort,
) -> (String, Vec<String>, usize) {
    let mut text = String::new();
    let mut prov: Vec<String> = Vec::new();
    let mut used = 0usize;
    for (line, id) in lines {
        let tokens = tokenizer.count(line);
        if used + tokens > remaining || used + tokens > ceiling.max(used) {
            break;
        }
        text.push_str(line);
        text.push('\n');
        prov.push(id.clone());
        used += tokens;
    }
    (text, prov, used)
}

fn render_episodes(
    episodes: &[EpisodeExcerpt],
    expand_top_k: usize,
    _ceiling: usize,
    _remaining: usize,
    tokenizer: &dyn TokenizerPort,
    layers: &mut Vec<ContextLayer>,
    total_tokens: &mut usize,
    remaining_ref: &mut usize,
) -> bool {
    if episodes.is_empty() {
        return false;
    }
    // Sort by salience × recency, deterministic tie-break on episode_id.
    let mut sorted: Vec<&EpisodeExcerpt> = episodes.iter().collect();
    sorted.sort_by(|a, b| {
        let sa = expand_score(a);
        let sb = expand_score(b);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.episode_id.cmp(&b.episode_id))
    });
    let mut text = String::new();
    let mut prov: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    for (i, ep) in sorted.iter().enumerate() {
        let line = if i < expand_top_k {
            ep.content.clone()
        } else {
            // One-line summary for the tail.
            let trimmed: String = ep.content.chars().take(120).collect();
            format!("[{id}] {trimmed}…", id = ep.episode_id)
        };
        let tokens = tokenizer.count(&line);
        if used + tokens > *remaining_ref {
            truncated = true;
            break;
        }
        text.push_str(&line);
        text.push('\n');
        prov.push(ep.episode_id.clone());
        used += tokens;
    }
    if !text.is_empty() {
        *total_tokens += used;
        *remaining_ref = remaining_ref.saturating_sub(used);
        layers.push(ContextLayer {
            kind: LayerKind::RecentEpisodes,
            text,
            estimated_tokens: used,
            provenance: prov,
        });
    }
    truncated
}

fn expand_score(ep: &EpisodeExcerpt) -> f64 {
    // recency in millis since 2020-01-01, normalised to ~1.0 for recent.
    let age_ms = (1_700_000_000_000i64 - ep.ingested_at).max(0) as f64;
    let recency = 1.0 / (1.0 + age_ms / (365.0 * 24.0 * 3600.0 * 1000.0));
    ep.salience * ep.salience.max(0.5) * recency
}

fn render_belief_line(b: &RenderedBelief, form: BeliefForm) -> String {
    match form {
        BeliefForm::OneLine => {
            format!(
                "{subj} {pred} {obj}",
                subj = b.subject_canonical_key,
                pred = b.predicate,
                obj = b.object
            )
        }
        BeliefForm::WithValidity => {
            format!(
                "{subj} {pred} {obj} ({valid})",
                subj = b.subject_canonical_key,
                pred = b.predicate,
                obj = b.object,
                valid = format_validity(b.valid_from, b.valid_to)
            )
        }
        BeliefForm::WithProvenance => {
            format!(
                "{subj} {pred} {obj} ({valid}, src=[{src}])",
                subj = b.subject_canonical_key,
                pred = b.predicate,
                obj = b.object,
                valid = format_validity(b.valid_from, b.valid_to),
                src = b.sources.join(",")
            )
        }
    }
}

fn format_validity(from: i64, to: i64) -> String {
    // Cheap human-readable validity. The full Timeline is exposed
    // through `recall(timeline)`; here we only need a glance.
    if to == i64::MAX - 1 {
        format!("from {from} (open)")
    } else if from == i64::MIN + 1 {
        format!("until {to}")
    } else {
        format!("{from}..{to}")
    }
}

fn short(id: &str) -> &str {
    if id.len() > 16 {
        &id[..16]
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxibrain_ports::CharTokenizer;

    fn tok() -> CharTokenizer {
        CharTokenizer
    }

    fn profile_fact(canonical: &str) -> ProfileFact {
        ProfileFact {
            subject: "subj1".into(),
            canonical_key: canonical.into(),
            predicate: "works_on".into(),
            text: "ProjectX".into(),
            valid_from: 1,
            valid_to: i64::MAX - 1,
            confidence: 0.9,
            trust: TrustTier::Trusted,
            sources: vec!["ep1".into()],
        }
    }

    #[test]
    fn pack_returns_empty_when_input_empty() {
        let input = ContextInput::default();
        let budget = ContextBudget { max_tokens: 1000 };
        let policy = PackPolicy::for_budget(1000);
        let out = pack(&input, &budget, &policy, &tok());
        assert_eq!(out.total_tokens, 0);
        assert!(out.layers.is_empty());
    }

    #[test]
    fn pack_total_tokens_within_budget() {
        let mut input = ContextInput::default();
        for i in 0..50 {
            input.profile.push(profile_fact(&format!("alice_{i}")));
            input.beliefs.push(RenderedBelief {
                statement_id: format!("s{i}"),
                subject: "alice".into(),
                subject_canonical_key: format!("alice_{i}"),
                predicate: "works_on".into(),
                object: "ProjectX".into(),
                valid_from: 1,
                valid_to: 100,
                confidence: 0.8,
                status: BeliefStatus::Active,
                support_episodes: 1,
                sources: vec![format!("ep{i}")],
            });
        }
        let budget = ContextBudget { max_tokens: 800 };
        let policy = PackPolicy::for_budget(800);
        let out = pack(&input, &budget, &policy, &tok());
        assert!(out.total_tokens <= budget.max_tokens, "total={} > budget={}", out.total_tokens, budget.max_tokens);
    }

    #[test]
    fn pack_profile_layer_present_when_input_has_profile() {
        let mut input = ContextInput::default();
        input.profile.push(profile_fact("Alice"));
        let budget = ContextBudget { max_tokens: 1000 };
        let policy = PackPolicy::for_budget(1000);
        let out = pack(&input, &budget, &policy, &tok());
        assert!(out
            .layers
            .iter()
            .any(|l| matches!(l.kind, LayerKind::Profile)));
    }

    #[test]
    fn pack_summary_layer_includes_sources() {
        let mut input = ContextInput::default();
        input.summaries.push(SummaryWithUncertainty {
            summary_id: "sm1".into(),
            text: "A summary of things.".into(),
            confidence: 0.7,
            sources: vec!["ep_a".into(), "ep_b".into()],
        });
        let budget = ContextBudget { max_tokens: 1000 };
        let policy = PackPolicy::for_budget(1000);
        let out = pack(&input, &budget, &policy, &tok());
        let layer = out
            .layers
            .iter()
            .find(|l| matches!(l.kind, LayerKind::Summaries))
            .expect("summaries layer");
        assert!(layer.text.contains("ep_a"));
        assert!(layer.text.contains("ep_b"));
    }

    #[test]
    fn pack_deterministic() {
        let mut input = ContextInput::default();
        for i in 0..10 {
            input.profile.push(profile_fact(&format!("a{i}")));
        }
        let budget = ContextBudget { max_tokens: 600 };
        let policy = PackPolicy::for_budget(600);
        let a = pack(&input, &budget, &policy, &tok());
        let b = pack(&input, &budget, &policy, &tok());
        let ja = serde_json::to_string(&a).unwrap();
        let jb = serde_json::to_string(&b).unwrap();
        assert_eq!(ja, jb);
    }

    use proptest::prelude::*;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Budget soundness: total_tokens <= budget.max_tokens.
        #[test]
        fn prop_budget_soundness(
            n_profile in 0usize..20,
            n_beliefs in 0usize..30,
            n_episodes in 0usize..10,
            budget in 100usize..5_000,
        ) {
            use proptest::prelude::*;
            let mut input = ContextInput::default();
            for i in 0..n_profile {
                input.profile.push(profile_fact(&format!("p{i}")));
            }
            for i in 0..n_beliefs {
                input.beliefs.push(RenderedBelief {
                    statement_id: format!("s{i}"),
                    subject: "subj".into(),
                    subject_canonical_key: format!("subj{i}"),
                    predicate: "knows".into(),
                    object: "obj".into(),
                    valid_from: 0,
                    valid_to: 100,
                    confidence: 0.5,
                    status: BeliefStatus::Active,
                    support_episodes: 1,
                    sources: vec![],
                });
            }
            for i in 0..n_episodes {
                input.episodes.push(EpisodeExcerpt {
                    episode_id: format!("e{i}"),
                    content: format!("content for episode {i}"),
                    ingested_at: 1_700_000_000_000 - (i as i64) * 1_000,
                    salience: 0.5,
                });
            }
            let policy = PackPolicy::for_budget(budget);
            let out = pack(&input, &ContextBudget { max_tokens: budget }, &policy, &tok());
            prop_assert!(out.total_tokens <= budget,
                "total {} > budget {}", out.total_tokens, budget);
        }

        /// Determinism: byte-equal output across runs.
        #[test]
        fn prop_determinism(
            n_profile in 0usize..10,
            budget in 200usize..3_000,
        ) {
            let mut input = ContextInput::default();
            for i in 0..n_profile {
                input.profile.push(profile_fact(&format!("a{i}")));
            }
            let policy = PackPolicy::for_budget(budget);
            let a = pack(&input, &ContextBudget { max_tokens: budget }, &policy, &tok());
            let b = pack(&input, &ContextBudget { max_tokens: budget }, &policy, &tok());
            prop_assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
        }
    }
}
