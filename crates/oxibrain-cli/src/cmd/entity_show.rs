use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::BeliefStatus;
use std::path::Path;

pub async fn run(dir: &Path, id: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let beliefs = brain.beliefs(&space_id, id).await?;
    println!("entity {id}: {} beliefs", beliefs.len());
    for b in &beliefs {
        let status = match b.status {
            BeliefStatus::Active => "active",
            BeliefStatus::Superseded => "superseded",
            BeliefStatus::Contradicted => "contradicted",
            BeliefStatus::Retracted => "retracted",
        };
        println!(
            "  [{}] statement={} confidence={:.3} from={} to={}",
            status,
            b.statement,
            b.confidence,
            b.valid_from.millis(),
            b.valid_to.millis()
        );
    }
    Ok(())
}
