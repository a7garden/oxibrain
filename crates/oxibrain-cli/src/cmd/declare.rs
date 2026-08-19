use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::Declaration;
use std::path::Path;

pub async fn run(dir: &Path, json: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl: Declaration =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse declaration: {e}"))?;
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("declared as episode: {ep_id}");
    Ok(())
}
