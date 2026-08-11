use oxibrain::{Brain, BrainConfig, RedactTarget};
use std::path::Path;

pub async fn run(
    dir: &Path,
    target: &str,
    space: &str,
    dry_run: bool,
    reason: &str,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let parsed = parse_target(target, &space_id)?;
    if dry_run {
        let closure = brain.redact_dry_run(&parsed).await?;
        print_closure("dry-run", &closure);
        Ok(())
    } else {
        let result = brain.redact(&parsed, reason, "cli").await?;
        print_closure("redacted", &result.closure);
        println!("beliefs refolded: {}", result.beliefs_refolded);
        Ok(())
    }
}

fn parse_target(target: &str, space_id: &str) -> anyhow::Result<RedactTarget> {
    let (kind, id) = target
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("target must be `kind:id`, got `{target}`"))?;
    let id = id.trim();
    if id.is_empty() {
        anyhow::bail!("target id is empty in `{target}`");
    }
    match kind {
        "episode" => Ok(RedactTarget::Episode { id: id.to_string() }),
        "entity" => Ok(RedactTarget::Entity {
            space: space_id.to_string(),
            entity_id: id.to_string(),
        }),
        "predicate" => {
            let (entity_id, predicate) = id
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("predicate target must be `entity/predicate`"))?;
            Ok(RedactTarget::PredicateScoped {
                space: space_id.to_string(),
                entity_id: entity_id.to_string(),
                predicate: predicate.to_string(),
            })
        }
        other => anyhow::bail!("unknown target kind `{other}` (expected episode|entity|predicate)"),
    }
}

fn print_closure(label: &str, c: &oxibrain::RedactionClosure) {
    println!("{label}:");
    println!("  episodes: {}", c.episodes.len());
    for id in &c.episodes {
        println!("    episode:{id}");
    }
    println!("  assertions: {}", c.assertions.len());
    println!("  statements: {}", c.statements.len());
    for id in &c.statements {
        println!("    statement:{id}");
    }
    println!("  mentions: {}", c.mentions.len());
    println!("  extractions: {}", c.extractions.len());
    println!("  summaries: {}", c.summaries.len());
}
