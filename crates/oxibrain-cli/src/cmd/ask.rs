use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::retrieval::{Query, QueryMode, SearchTarget};
use std::path::Path;

pub async fn run(dir: &Path, question: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let q = Query {
        text: question.to_string(),
        mode: QueryMode::Hybrid,
        space: space_id,
        as_of: None,
        limit: 20,
        min_confidence: 0.0,
    };
    let result = brain.query(q).await?;
    println!(
        "hits: {} (total found: {})",
        result.items.len(),
        result.total_found
    );
    for item in &result.items {
        let target = match &item.target {
            SearchTarget::Episode { id } => format!("episode:{id}"),
            SearchTarget::Statement { id } => format!("statement:{id}"),
            SearchTarget::Entity { id } => format!("entity:{id}"),
        };
        println!(
            "  rank={} score={:.4} salience={:.4} -> {target}",
            item.rank, item.fused_score, item.salience
        );
    }
    Ok(())
}
