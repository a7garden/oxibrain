use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let report = brain.reproject().await?;
    println!("reprojection complete");
    println!(
        "declarations replayed: {}, skipped: {}",
        report.declarations_replayed, report.declarations_skipped
    );
    if report.declarations_skipped > 0 {
        println!(
            "warning: {} declaration episode(s) could not be parsed and were skipped (left untouched in the ledger)",
            report.declarations_skipped
        );
    }
    Ok(())
}
