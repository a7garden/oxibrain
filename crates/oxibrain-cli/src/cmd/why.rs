use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::{DropReason, Query, QueryMode};
use std::path::Path;

/// `oxibrain why --dropped "<query>"` — print what `rank` discarded for a
/// query, from the conservation guarantee (DESIGN §11.8). Empty output
/// means nothing was dropped.
pub async fn run_dropped(
    dir: &Path,
    query_text: &str,
    space: &str,
    min_confidence: f32,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let q = Query {
        text: query_text.to_string(),
        mode: QueryMode::Hybrid,
        space: space_id,
        as_of: None,
        limit: 20,
        min_confidence,
    };
    let result = brain.query(q).await?;
    if result.dropped.is_empty() {
        println!(
            "no candidates were dropped (limit {})",
            result.total_candidates
        );
        return Ok(());
    }
    println!(
        "dropped {} of {} candidates:",
        result.dropped.len(),
        result.total_candidates
    );
    for d in &result.dropped {
        let reason = match &d.reason {
            DropReason::BelowConfidenceFloor { actual, floor } => {
                format!("below confidence floor: {actual:.2} < {floor:.2}")
            }
            DropReason::OutsideValidWindow { valid_at } => {
                format!("outside valid window at {valid_at:?}")
            }
            DropReason::BeforeKnownAt {
                known_at,
                recorded_at,
            } => {
                format!("recorded {recorded_at:?} after known_at {known_at:?}")
            }
            DropReason::TrustExcluded { tier } => format!("trust tier excluded: {tier:?}"),
            DropReason::PredicateDenied { predicate } => {
                format!("predicate denied: {predicate}")
            }
            DropReason::EntityTypeMismatch { expected } => {
                format!("entity type mismatch, expected {expected:?}")
            }
            DropReason::TruncatedByBudget { position } => {
                format!("truncated by budget (position {position})")
            }
        };
        println!("  {} — {reason}", d.target.rrf_key());
    }
    Ok(())
}

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
