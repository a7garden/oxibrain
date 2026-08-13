use oxibrain::{Brain, BrainConfig};
use std::path::Path;

/// `oxibrain page <entity>` — render an entity page (brief) to stdout
/// (ARCHITECTURE.md §16.4, M9 §9.4).
pub async fn run(dir: &Path, entity: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let page = brain.brief(&space_id, entity).await?;
    println!("{page}");
    Ok(())
}
