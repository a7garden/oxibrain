use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let episodes = brain.episode_count().await?;
    println!("dir:    {}", dir.display());
    println!("episodes: {episodes}");

    // Documents plane inventory (configured roots + cached files).
    let (roots, files) = brain.document_counts().await?;
    println!("documents: {roots} root(s), {files} file(s)");

    // Memory-plane extraction backlog: how much `extract --pending` would do.
    let pending = brain.pending_extraction_stats().await?;
    match pending.oldest_seq {
        Some(seq) => println!(
            "pending extraction: {} episode(s), oldest seq {seq}",
            pending.count
        ),
        None => println!("pending extraction: none"),
    }
    Ok(())
}
