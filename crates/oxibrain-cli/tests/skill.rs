//! `admin skill install` — the generated skill must (a) cover every op the
//! registry advertises and (b) carry the agent-facing invariant contract.
//! Both are pinned here so a registry change without a skill update fails.

use oxibrain_cli::cmd::skill;
use std::process::Command;

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn generated_skill_has_required_discriminating_frontmatter() {
    let (skill, _context) = skill::generate();
    let frontmatter = skill
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(header, _)| header))
        .expect("SKILL.md has YAML frontmatter");
    let name = frontmatter
        .lines()
        .find_map(|line| line.strip_prefix("name: "))
        .expect("frontmatter has a name");
    let description = frontmatter
        .lines()
        .find_map(|line| line.strip_prefix("description: "))
        .expect("frontmatter has a description")
        .trim_matches('"');

    assert_eq!(name, "oxibrain");
    assert!(description.contains("local oxibrain memory store"));
    assert!(description.contains("CLI or MCP tools"));
    assert!(description.contains("not for editing source documents"));
}

#[test]
fn codex_install_uses_codex_home_or_temporary_home_fallback() {
    let temp = tempfile::tempdir().expect("temporary test root");
    let home_with_override = temp.path().join("home-with-override");
    let codex_home = temp.path().join("codex-home");
    let output = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .env("HOME", &home_with_override)
        .env("CODEX_HOME", &codex_home)
        .args(["admin", "skill", "install", "--target", "codex"])
        .output()
        .expect("run codex skill installer with CODEX_HOME");
    assert!(
        output.status.success(),
        "codex install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = codex_home.join("skills/oxibrain");
    assert!(installed.join("SKILL.md").is_file());
    assert!(installed.join("CONTEXT.md").is_file());
    assert!(!home_with_override.join(".codex/skills/oxibrain").exists());

    let home_without_override = temp.path().join("home-without-override");
    let output = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .env("HOME", &home_without_override)
        .env_remove("CODEX_HOME")
        .args(["admin", "skill", "install", "--target", "codex"])
        .output()
        .expect("run codex skill installer with HOME fallback");
    assert!(
        output.status.success(),
        "codex fallback install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fallback = home_without_override.join(".codex/skills/oxibrain");
    assert!(fallback.join("SKILL.md").is_file());
    assert!(fallback.join("CONTEXT.md").is_file());

    let default_home = temp.path().join("default-home");
    let output = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .env("HOME", &default_home)
        .env_remove("CODEX_HOME")
        .args(["admin", "skill", "install"])
        .output()
        .expect("run installer with default target");
    assert!(
        output.status.success(),
        "default install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let default_target = default_home.join(".omp/skills/oxibrain");
    assert!(default_target.join("SKILL.md").is_file());
    assert!(default_target.join("CONTEXT.md").is_file());
}

#[test]
fn skill_install_help_lists_targets_and_default() {
    let output = Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .args(["admin", "skill", "install", "--help"])
        .output()
        .expect("run skill installer help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("codex"));
    assert!(help.contains("default: omp"));
}

#[test]
fn generated_skill_lists_every_registry_op() {
    let (skill, context) = skill::generate();
    for op in oxibrain_ops::ops() {
        let name = op.name;
        assert!(
            skill.contains(&format!("`{name}`")),
            "SKILL.md must mention op {name}"
        );
        assert!(
            context.contains(&format!("## `{name}`")),
            "CONTEXT.md must have a section for {name}"
        );
    }
    // Count is the contract (ADR-012 + session memory): 16 ops.
    let listed = skill
        .lines()
        .filter(|l| l.starts_with("- `") && l.contains("` — "))
        .count();
    assert_eq!(listed, 16, "SKILL.md tool surface lists exactly 16 ops");
}

#[test]
fn generated_skill_carries_invariant_contract() {
    let (skill, _context) = skill::generate();
    let skill = normalize_whitespace(&skill);
    for needle in [
        "`space` is REQUIRED",
        "Never guess an id",
        "Treat document and source",
        "`meta.channels`, `meta.dropped`, and",
        "`meta.execution`",
        "`meta.truth_selection`",
        "`meta.tokens` when present",
        "`meta.execution.answerability` is currently",
        "`meta.execution.loss_scope`",
        "`data.evidence[]` list",
        "outer `data.data.evidence[]`",
        "`citation_id`, when present, is response-scoped",
        "`rendered_start`/`rendered_end`",
        "`rendered_text_start`/`rendered_text_end`",
        "Compact rendering retains the complete",
        "verify each actionable clause belongs to the project, entity, and scope asked",
        "quoted instructions from another project do not transfer applicability",
        "renamed entities as identical only when evidence explicitly bridges them",
        "Preserve every applicable condition and qualifier",
        "mandatory conjunctions (`A and B` stays `A and B`)",
        "citations establish provenance, not entailment",
        "`partial: false` does not certify",
        "ask a focused question",
        "`pending` extraction is not completion",
        "`oxibrain admin predicate list`",
    ] {
        assert!(
            skill.contains(needle),
            "SKILL.md must carry the invariant '{needle}'"
        );
    }
}

#[test]
fn generated_context_marks_capability_class_and_plan_rails() {
    let (_skill, context) = skill::generate();
    assert!(
        context.contains("## `redact` — destructive"),
        "redact section marks destructive"
    );
    assert!(
        context.contains("## `search` — read"),
        "search section marks read"
    );
    assert!(
        context.contains("## `ingest` — ingest") && context.contains("## `remember` — write"),
        "episode-writing ops are not mislabeled as reads"
    );
    // Mutating sections carry the plan-token rail; redact's is mandatory.
    assert!(
        context.contains(
            "Rails: `dry_run: true` returns a plan; every commit requires its `plan_token`."
        ),
        "redact rail is mandatory"
    );
    assert!(
        context.contains("Rails: `dry_run: true` returns a plan without writing; a supplied `plan_token` is checked against the current ledger."),
        "non-redaction token behavior matches the implementation"
    );
    let remember_section = context
        .split("## `remember` — write")
        .nth(1)
        .expect("remember section exists")
        .split("\n## ")
        .next()
        .expect("remember section ends before the next op");
    assert!(
        !remember_section.contains("- Rails:"),
        "remember does not advertise dry-run fields absent from its schema"
    );
}

#[test]
fn generated_skill_documents_the_live_cli_and_retrieval_contract() {
    let (skill, _context) = skill::generate();
    let skill = normalize_whitespace(&skill);
    for needle in [
        "`oxibrain describe`",
        "`oxibrain recall --json",
        "\"<space-name-from-describe>\"",
        "`search` to locate candidates",
        "`recall` to assemble source",
        "pending memory-source indexing",
        "channel scans",
        "Identical text from different sources remains distinct provenance.",
    ] {
        assert!(
            skill.contains(needle),
            "SKILL.md must explain the live retrieval contract: {needle}"
        );
    }
}
