use oxibrain::Brain;
use oxibrain::BrainConfig;
use std::path::Path;

pub async fn run(dir: &Path, statement_id: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let block = brain.why(&space_id, statement_id).await?;
    println!("statement: {}", block.statement.id);
    println!("  subject: {}", block.statement.subject);
    println!("  predicate: {}", block.statement.predicate);
    println!("  object: {:?}", block.statement.object);
    println!("status: {}", block.status);
    println!(
        "confidence: raw={:.3} support={} contradiction={}",
        block.confidence_breakdown.raw_confidence,
        block.confidence_breakdown.support_count,
        block.confidence_breakdown.contradiction_count
    );
    println!("assertions: {}", block.assertions.len());
    for a in &block.assertions {
        println!(
            "  [{}] episode={} extractor={:?} conf={:.3} recorded={}",
            a.polarity, a.episode_id, a.extractor, a.confidence, a.recorded_at
        );
    }
    Ok(())
}
