use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::{Declaration, EntityRef};
use std::path::Path;

pub async fn run(dir: &Path, surface: &str, ty: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl = Declaration::Split {
        entity: EntityRef {
            surface: surface.to_string(),
            ty: ty.to_string(),
        },
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("split as episode: {ep_id}");
    Ok(())
}
