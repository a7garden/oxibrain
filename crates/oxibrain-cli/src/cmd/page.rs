use oxibrain::{Brain, BrainConfig, BriefTarget};
use std::path::Path;

/// `oxibrain page <entity> [--kind entity|space|topic] [--topic <kw>]`
/// — render a brief page to stdout (ARCHITECTURE.md §16.4, M9 §14.1).
/// `--kind entity` (default) renders an entity page; `--kind space` renders
/// a space overview; `--kind topic <kw>` keyword-searches entity surfaces.
pub async fn run(
    dir: &Path,
    entity: Option<&str>,
    space: &str,
    kind: &str,
    topic: Option<&str>,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let page = match kind {
        "entity" => {
            let id = entity
                .ok_or_else(|| anyhow::anyhow!("page: --kind entity requires an entity id"))?;
            brain.brief(&space_id, id).await?
        }
        "space" => brain.brief_target(&space_id, BriefTarget::Space).await?,
        "topic" => {
            let t = topic
                .ok_or_else(|| anyhow::anyhow!("page: --kind topic requires --topic <keyword>"))?;
            brain.brief_target(&space_id, BriefTarget::Topic(t)).await?
        }
        other => anyhow::bail!("page: --kind '{other}' (expected entity|space|topic)"),
    };
    println!("{page}");
    Ok(())
}
