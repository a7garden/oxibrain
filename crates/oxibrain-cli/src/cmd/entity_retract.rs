use oxibrain::Brain;
use oxibrain::BrainConfig;
#[allow(unused_imports)]
use oxibrain_store::project::{DeclObject, Declaration};
use std::path::Path;

pub async fn run(dir: &Path, statement_id: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let (subject, predicate, object) = brain.retract_parts(&space_id, statement_id).await?;
    // Find the originating episode for audit context.
    let episode = String::new(); // retract_parts doesn't return episode; use empty.
    let decl = Declaration::Retract {
        subject,
        predicate,
        object,
        episode,
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("retracted as episode: {ep_id}");
    Ok(())
}
