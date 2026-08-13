use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::TargetId;
use std::path::Path;

pub async fn run(dir: &Path, question: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let q = oxibrain_core::Query {
        text: question.to_string(),
        mode: oxibrain_core::QueryMode::Hybrid,
        space: space_id,
        as_of: None,
        limit: 20,
        min_confidence: 0.0,
    };
    let result = brain.query(q).await?;
    println!(
        "hits: {} (total candidates: {})",
        result.items.len(),
        result.total_candidates
    );
    for item in &result.items {
        let target = match &item.target {
            TargetId::Episode { id } => format!("episode:{id}"),
            TargetId::Statement { id } => format!("statement:{id}"),
            TargetId::Entity { id } => format!("entity:{id}"),
            TargetId::Chunk { id } => format!("chunk:{id}"),
            TargetId::Community { id } => format!("community:{id}"),
        };
        println!(
            "  rank={} score={:.4} salience={:.4} -> {target}",
            item.rank, item.fused_score, item.salience
        );
    }
    Ok(())
}
