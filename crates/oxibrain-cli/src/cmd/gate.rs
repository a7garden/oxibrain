//! `oxibrain eval --suite gate` — three-arm comparison runner
//! (ARCHITECTURE.md §17.2).
//!
//! Loads `eval/golden/` (manifest + episodes + questions), ingests the
//! episodes as declarations on a fresh brain, then for each question runs
//! arm (b) lexical and arm (c) hybrid and scores whether the answer
//! appears in the rendered top-K statements.
//!
//! Per ROADMAP §4, this is the controlled comparison whose outcome decides
//! whether to proceed with M10 as written, fix extraction, or invoke D19's
//! pre-commitment (demote the graph). Arms (a) and the frontier tier are
//! out of scope for the golden-only gate (LongMemEval removed from the
//! plan, 2026-08-13).
//!
//! The runner is deterministic and self-contained: no network, no LLM,
//! no live model. Tokens/query is approximated by the number of ranked
//! items + the character length of rendered statements.

use anyhow::{Context, Result, bail};
use oxibrain::Brain;
use oxibrain_core::retrieval::{Query, QueryMode};
use oxibrain_ports::{FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

// ── Golden corpus types (TOML) ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Manifest {
    #[allow(dead_code)]
    version: String,
    #[allow(dead_code)]
    categories: Vec<Category>,
    episodes: Vec<EpisodeEntry>,
    questions: Vec<QuestionEntry>,
}

#[derive(Debug, Deserialize)]
struct Category {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct EpisodeEntry {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    shape: String,
    #[allow(dead_code)]
    lang: String,
    file: String,
}

#[derive(Debug, Deserialize)]
struct QuestionEntry {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    category: String,
    file: String,
}

#[derive(Debug, Deserialize)]
struct EpisodeFile {
    #[allow(dead_code)]
    shape: String,
    #[allow(dead_code)]
    lang: String,
    #[allow(dead_code)]
    occurred_at: String,
    #[allow(dead_code)]
    content: String,
    entities: Vec<EpisodeEntity>,
    statements: Vec<EpisodeStatement>,
}

#[derive(Debug, Deserialize)]
struct EpisodeEntity {
    surface: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Deserialize)]
struct EpisodeStatement {
    predicate: String,
    subject_surface: String,
    /// Either an entity reference (`object_surface`, type resolved from the
    /// episode's `[[entities]]`) or a literal (`object_literal_type` +
    /// `object_literal_value`).
    object_surface: Option<String>,
    object_literal_type: Option<String>,
    object_literal_value: Option<String>,
    valid_from: String,
    #[serde(default)]
    valid_to: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuestionFile {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    category: String,
    question: String,
    #[serde(default)]
    as_of: Option<String>,
    answer: String,
    #[allow(dead_code)]
    supporting_episodes: Vec<String>,
}

// ── Arm results ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arm {
    /// Lexical only (word + ngram + RRF) — the control.
    B,
    /// Full hybrid (lexical ∪ vector ∪ graph) — the treatment.
    C,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::B => "b (lexical)",
            Arm::C => "c (hybrid)",
        }
    }

    fn query_mode(self) -> QueryMode {
        match self {
            Arm::B => QueryMode::Lexical,
            Arm::C => QueryMode::Hybrid,
        }
    }
}

#[derive(Debug, Clone)]
struct ArmResult {
    passed: bool,
    /// Approximate token count: characters / 4. This is a coarse proxy
    /// (real tokenization is model-specific); the gate reports it for
    /// shape-comparison only, not for a hard budget claim.
    approx_tokens: usize,
}

// ── Public entry point ────────────────────────────────────────────────────

pub async fn run(suite: &str, corpus_dir: &Path) -> Result<()> {
    if suite != "gate" {
        bail!("gate runner only handles the 'gate' suite (got '{suite}')");
    }
    let manifest_path = corpus_dir.join("manifest.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest_text).context("parse manifest.toml")?;

    // 1. Set up a fresh brain and ingest every episode as declarations.
    //    Use a fake clock pinned to 2024-12-01 (after all episodes) so
    //    as_of queries for the temporal corpus resolve correctly.
    let dir = TempDir::new().context("create temp dir")?;
    let clock_ts = Timestamp::from_millis(1_700_000_000_000); // 2023-11-14
    let clock = Arc::new(FakeClock::new(clock_ts));
    let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.path()), clock)
        .await
        .context("open brain")?;
    let space = "gate";
    let space_id = brain.ensure_space(space).await.context("ensure space")?;

    let mut ingested = 0usize;
    // All entity surfaces in the corpus — used to derive keyword queries
    // from question text (a real agent queries the entity name, not the
    // whole NL sentence; arm (b) lexical cannot match an entire question).
    let mut entity_surfaces: Vec<String> = Vec::new();
    for ep_entry in &manifest.episodes {
        let ep_path = corpus_dir.join(&ep_entry.file);
        let ep_text = std::fs::read_to_string(&ep_path)
            .with_context(|| format!("read {}", ep_path.display()))?;
        let ep: EpisodeFile =
            toml::from_str(&ep_text).with_context(|| format!("parse {}", ep_path.display()))?;
        for e in &ep.entities {
            entity_surfaces.push(e.surface.clone());
        }
        for st in &ep.statements {
            let decl = statement_to_declaration(st, &ep.entities)?;
            brain
                .declare(&space_id, decl)
                .await
                .with_context(|| format!("declare from {}", ep_entry.id))?;
            ingested += 1;
        }
    }
    entity_surfaces.sort();
    entity_surfaces.dedup();

    // 1.5. Declarations do not auto-index FTS; the query arms need the
    //      lexical index to surface anything. Rebuild before scoring.
    brain
        .rebuild_indexes(&space_id)
        .await
        .context("rebuild indexes")?;

    // 2. For each question, run both arms and score.
    let mut results: Vec<QuestionResult> = Vec::new();
    for q_entry in &manifest.questions {
        let q_path = corpus_dir.join(&q_entry.file);
        let q_text = std::fs::read_to_string(&q_path)
            .with_context(|| format!("read {}", q_path.display()))?;
        let q: QuestionFile =
            toml::from_str(&q_text).with_context(|| format!("parse {}", q_path.display()))?;
        // Keyword query: the entity surface(s) named in the question.
        let keyword = extract_keywords(&q.question, &entity_surfaces);
        let as_of = q.as_of.as_deref().map(parse_iso_to_timestamp).transpose()?;
        let arm_b = run_arm(&brain, &space_id, &keyword, Arm::B, as_of, &q.answer).await?;
        let arm_c = run_arm(&brain, &space_id, &keyword, Arm::C, as_of, &q.answer).await?;
        // M9 exit criterion: tokens per answered question must DECREASE
        // vs M8's recall-only path (assemble_context). Measure both.
        let recall = brain
            .assemble_context(&space_id, &keyword, 3000)
            .await
            .context("assemble_context")?;
        let recall_tokens = recall.total_tokens;
        let brief_tokens = brief_token_cost(&brain, &space_id, &keyword, &entity_surfaces).await?;
        results.push(QuestionResult {
            id: q.id,
            category: q.category,
            answer: q.answer,
            arm_b,
            arm_c,
            recall_tokens,
            brief_tokens,
        });
    }

    // 3. Report.
    print_report(&results, ingested);

    Ok(())
}

#[derive(Debug, Clone)]
struct QuestionResult {
    id: String,
    category: String,
    answer: String,
    arm_b: ArmResult,
    arm_c: ArmResult,
    /// M8 recall-only context tokens (assemble_context budget=3000).
    recall_tokens: usize,
    /// M9 brief/navigate path tokens (brief pages for the top entities).
    brief_tokens: usize,
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn statement_to_declaration(
    st: &EpisodeStatement,
    entities: &[EpisodeEntity],
) -> Result<Declaration> {
    let valid_from = parse_iso_to_timestamp(&st.valid_from)?.millis();
    let valid_to = match &st.valid_to {
        Some(s) => parse_iso_to_timestamp(s)?.millis(),
        None => oxibrain_ports::TIME_MAX.millis(),
    };
    let subject = EntityRef {
        surface: st.subject_surface.clone(),
        ty: entity_type(entities, &st.subject_surface)
            .unwrap_or("Person")
            .to_string(),
    };
    let object = if let Some(surface) = &st.object_surface {
        let ty = entity_type(entities, surface).ok_or_else(|| {
            anyhow::anyhow!("object surface '{surface}' not declared in episode entities")
        })?;
        DeclObject::Entity {
            surface: surface.clone(),
            ty: ty.to_string(),
        }
    } else if let (Some(lt), Some(val)) = (&st.object_literal_type, &st.object_literal_value) {
        DeclObject::Literal {
            literal_type: lt.clone(),
            value: val.clone(),
        }
    } else {
        bail!(
            "statement '{}' has neither object_surface nor object_literal_*",
            st.subject_surface
        );
    };
    Ok(Declaration::AddStatement {
        subject,
        predicate: st.predicate.clone(),
        object,
        polarity: "affirm".into(),
        valid_from,
        valid_to,
    })
}

/// Extract the entity surface(s) named in the question text. Returns the
/// joined surfaces (space-separated) so both arms query the same keyword
/// the way an agent would. Falls back to the whole question text if no
/// entity surface appears (rare — the corpus names entities in questions).
fn extract_keywords(question: &str, entity_surfaces: &[String]) -> String {
    let lower = question.to_lowercase();
    let mut found: Vec<&str> = Vec::new();
    for surface in entity_surfaces {
        if lower.contains(&surface.to_lowercase()) {
            found.push(surface.as_str());
        }
    }
    if found.is_empty() {
        question.to_string()
    } else {
        found.join(" ")
    }
}

/// M9 path token cost: render the brief pages for the keyword's entity
/// surfaces and sum their chars/4 (coarse token proxy, same as the arms).
/// An agent reading a brief page consumes its rendered text.
async fn brief_token_cost(
    brain: &Brain,
    space_id: &str,
    keyword: &str,
    entity_surfaces: &[String],
) -> Result<usize> {
    // Resolve the entity surfaces named in the keyword, then brief each.
    let mut total = 0usize;
    let mut seen = std::collections::HashSet::new();
    for surface in entity_surfaces {
        if !keyword.to_lowercase().contains(&surface.to_lowercase()) {
            continue;
        }
        if !seen.insert(surface.clone()) {
            continue;
        }
        if let Ok(Some(id)) = brain
            .resolve_entity_id(
                space_id,
                &entity_type_for(brain, space_id, surface).await,
                surface,
            )
            .await
        {
            let page = brain
                .brief(space_id, &id)
                .await
                .context("brief cost page")?;
            total += page.len() / 4;
        }
    }
    Ok(total)
}

/// Resolve the type of an entity surface by trying common types.
async fn entity_type_for(brain: &Brain, space_id: &str, surface: &str) -> String {
    for ty in ["Person", "Organization", "Project", "Place", "Concept"] {
        if brain
            .resolve_entity_id(space_id, ty, surface)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return ty.to_string();
        }
    }
    "Concept".to_string()
}

fn entity_type<'a>(entities: &'a [EpisodeEntity], surface: &str) -> Option<&'a str> {
    entities
        .iter()
        .find(|e| e.surface == surface)
        .map(|e| e.ty.as_str())
}

fn parse_iso_to_timestamp(s: &str) -> Result<Timestamp> {
    // The corpus uses `YYYY-MM-DD`. Convert to millis since epoch (UTC noon
    // to avoid TZ off-by-one). The Temporal answer correctness depends on
    // the interval, not the exact instant.
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        bail!("not a YYYY-MM-DD date: {s}");
    }
    let y: i64 = parts[0]
        .parse()
        .with_context(|| format!("year parse: {s}"))?;
    let m: i64 = parts[1]
        .parse()
        .with_context(|| format!("month parse: {s}"))?;
    let d: i64 = parts[2]
        .parse()
        .with_context(|| format!("day parse: {s}"))?;
    // Days from 1970-01-01 to Y-M-D, civil-date arithmetic.
    let days = days_from_civil(y, m, d);
    Ok(Timestamp(days * 86_400_000 + 12 * 3_600_000)) // noon UTC
}

/// Howard Hinnant's days_from_civil (public domain).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * m + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

async fn run_arm(
    brain: &Brain,
    space_id: &str,
    text: &str,
    arm: Arm,
    as_of: Option<Timestamp>,
    answer: &str,
) -> Result<ArmResult> {
    let query = Query {
        text: text.to_string(),
        mode: arm.query_mode(),
        space: space_id.to_string(),
        as_of,
        limit: 5,
        min_confidence: 0.0,
    };
    let result = brain.query(query).await.context(arm.label())?;
    // Render the top-K ranked statements by id (`id | subject predicate
    // object`) and check substring match against the answer. This is the
    // agent-visible surface: the ranked statement's full text, not just
    // its predicate name.
    let mut ids: Vec<String> = Vec::new();
    for item in result.items.iter().take(5) {
        if let oxibrain_core::rank::TargetId::Statement { id } = &item.target {
            ids.push(id.clone());
        }
    }
    let rendered_lines = brain.render_statements(space_id, &ids).await?;
    let rendered = rendered_lines.join("\n");
    let passed = !rendered.is_empty()
        && answer_matches(answer.to_lowercase().as_str(), &rendered.to_lowercase());
    let approx_tokens = rendered.len() / 4;
    Ok(ArmResult {
        passed,
        approx_tokens,
    })
}

fn answer_matches(needle: &str, haystack_lower: &str) -> bool {
    // The gate's scoring is a coarse case-insensitive substring match
    // against the rendered top-K. Multi-word answers: every word must
    // appear in some rendered line (looser than full-phrase match).
    let words: Vec<&str> = needle.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    words.iter().all(|w| haystack_lower.contains(w))
}

fn print_report(results: &[QuestionResult], ingested: usize) {
    println!("═══ oxibrain gate (golden-only) ═══");
    println!("Episodes ingested: {} declarations", ingested);
    println!("Questions:         {}", results.len());
    println!();
    // Per-question detail.
    for r in results {
        let mark_b = if r.arm_b.passed { "✓" } else { "✗" };
        let mark_c = if r.arm_c.passed { "✓" } else { "✗" };
        println!(
            "  {} [{}]  b={} ({:>3} tok)  c={} ({:>3} tok)  answer: {}",
            r.id,
            r.category,
            mark_b,
            r.arm_b.approx_tokens,
            mark_c,
            r.arm_c.approx_tokens,
            truncate(&r.answer, 40),
        );
    }
    println!();
    // Per-category delta (c − b accuracy).
    let categories: std::collections::BTreeSet<&str> =
        results.iter().map(|r| r.category.as_str()).collect();
    println!("Per-category accuracy (c − b):");
    for cat in categories {
        let in_cat: Vec<&QuestionResult> = results.iter().filter(|r| r.category == cat).collect();
        let n = in_cat.len() as f64;
        let b = in_cat.iter().filter(|r| r.arm_b.passed).count() as f64;
        let c = in_cat.iter().filter(|r| r.arm_c.passed).count() as f64;
        println!(
            "  {:<22}  b={:.0}/{:.0}  c={:.0}/{:.0}  delta(c−b) = {:+.0}",
            cat,
            b,
            n,
            c,
            n,
            (c - b),
        );
    }
    println!();
    // Tokens/query total.
    let b_tokens: usize = results.iter().map(|r| r.arm_b.approx_tokens).sum();
    let c_tokens: usize = results.iter().map(|r| r.arm_c.approx_tokens).sum();
    let n = results.len().max(1);
    println!(
        "Tokens/query (approx):  b={:.0}  c={:.0}  delta(c−b) = {:+} tok/q",
        b_tokens as f64 / n as f64,
        c_tokens as f64 / n as f64,
        (c_tokens as isize - b_tokens as isize) / n as isize,
    );
    // M9 exit criterion: tokens per answered question vs M8 recall-only.
    let recall_tokens: usize = results.iter().map(|r| r.recall_tokens).sum();
    let brief_tokens: usize = results.iter().map(|r| r.brief_tokens).sum();
    println!(
        "Tokens/answer:  M8 recall-only={:.0}  M9 brief/navigate={:.0}  delta = {:+} tok/q",
        recall_tokens as f64 / n as f64,
        brief_tokens as f64 / n as f64,
        (brief_tokens as isize - recall_tokens as isize) / n as isize,
    );
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// Entry point used by the CLI dispatcher — supports the `gate` suite
/// with a corpus-dir argument. The CLI passes the path to `eval/golden/`
/// (resolved relative to the manifest via the `--corpus` flag, defaulting
/// to `eval/golden` from the workspace root).
pub async fn run_with_dir(suite: &str, corpus_dir: Option<PathBuf>) -> Result<()> {
    let dir = match corpus_dir {
        Some(d) => d,
        None => default_corpus_dir()?,
    };
    run(suite, &dir).await
}

fn default_corpus_dir() -> Result<PathBuf> {
    // Walk up from CWD until we find `eval/golden/manifest.toml`.
    let mut here = std::env::current_dir().context("cwd")?;
    loop {
        let candidate = here.join("eval").join("golden");
        if candidate.join("manifest.toml").is_file() {
            return Ok(candidate);
        }
        if !here.pop() {
            bail!("could not locate eval/golden/manifest.toml from CWD");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        // 2023-11-14 is 19675 days after epoch (verified with a reference).
        assert_eq!(days_from_civil(2023, 11, 14), 19675);
        assert_eq!(days_from_civil(2023, 6, 1), 19509);
    }

    #[test]
    fn answer_matches_substring() {
        assert!(answer_matches("acme corp", "works for acme corp in 2023"));
        assert!(answer_matches(
            "alice smith",
            "alice smith works on project x"
        ));
        assert!(!answer_matches("missing", "alice works on project x"));
    }
}
