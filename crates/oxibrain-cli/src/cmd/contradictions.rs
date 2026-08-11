use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let stmts = brain.contradictions(&space_id).await?;
    println!("contradictions: {}", stmts.len());
    for s in &stmts {
        println!("  statement={} {}/{:?}", s.id, s.predicate, s.object);
    }
    Ok(())
}
