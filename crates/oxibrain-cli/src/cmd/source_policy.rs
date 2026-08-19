use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::Declaration;
use std::path::Path;

pub async fn run(
    dir: &Path,
    name: &str,
    trust: &str,
    effective_from: Option<i64>,
    effective_to: Option<i64>,
    space: &str,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let now = brain.clock_now();
    let decl = Declaration::SetSourcePolicy {
        source_name: name.to_string(),
        trust: trust.to_string(),
        effective_from: effective_from.unwrap_or(now.millis()),
        effective_to,
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("policy set as episode: {ep_id}");
    Ok(())
}
