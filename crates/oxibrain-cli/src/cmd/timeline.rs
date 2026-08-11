use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, entity_id: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let entries = brain.timeline(&space_id, entity_id, None, None).await?;
    println!("timeline for entity {entity_id}: {} entries", entries.len());
    for e in &entries {
        println!(
            "  [{}] {}/{} = {} from={} to={} (recorded {})",
            e.status,
            e.predicate,
            e.object_repr,
            e.statement_id,
            e.valid_from.millis(),
            e.valid_to.millis(),
            e.recorded_at.millis(),
        );
    }
    Ok(())
}
