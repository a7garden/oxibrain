use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::{Declaration, EntityRef};
use std::path::Path;

pub async fn run(
    dir: &Path,
    loser: &str,
    loser_type: &str,
    winner: &str,
    winner_type: &str,
    space: &str,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl = Declaration::Merge {
        loser: EntityRef {
            surface: loser.to_string(),
            ty: loser_type.to_string(),
        },
        winner: EntityRef {
            surface: winner.to_string(),
            ty: winner_type.to_string(),
        },
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("merged as episode: {ep_id}");
    Ok(())
}
