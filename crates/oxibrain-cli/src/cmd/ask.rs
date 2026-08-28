use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::TargetId;
use oxibrain_core::retrieval::SearchPlane;
use std::path::Path;

/// `oxibrain ask` — one query path, two result kinds (spec §8).
///
/// Memory renders the full ranking envelope (episode targets carry their
/// content excerpt — an agent asking a question must see the matched text,
/// not an opaque id). Documents render the verbatim chunk with its revision.
/// The two lists are printed separately, never blended.
pub async fn run(dir: &Path, question: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = crate::cmd::space_id(&brain, space).await?;

    let base = || oxibrain_core::Query {
        text: question.to_string(),
        mode: oxibrain_core::QueryMode::Hybrid,
        space: space_id.clone(),
        as_of: None,
        limit: 20,
        min_confidence: 0.0,
        planes: Default::default(),
    };

    // Memory plane: full ranking (episodes, statements, entities) as before.
    let mut memory_q = base();
    memory_q.planes = [SearchPlane::Memory].into_iter().collect();
    let memory = brain.query(memory_q).await?;
    println!(
        "hits: {} (total candidates: {})",
        memory.items.len(),
        memory.total_candidates
    );
    for item in &memory.items {
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
        if let TargetId::Episode { id } = &item.target {
            if let Ok(Some(ep)) = brain.get_episode(id).await {
                let flat: String = ep.content.chars().filter(|c| *c != '\n').collect();
                let excerpt: String = flat.chars().take(160).collect();
                println!("    {excerpt}");
            }
        }
    }

    // Document plane: reconcile + lexical/vector search + materialization.
    let mut doc_q = base();
    doc_q.planes = [SearchPlane::Documents].into_iter().collect();
    let response = brain.search(doc_q).await?;
    if !response.documents.is_empty() {
        println!("document hits: {}", response.documents.len());
        for hit in &response.documents {
            println!(
                "  score={:.4} {}:{} @{} (modified {})",
                hit.score,
                hit.root,
                hit.locator,
                &hit.revision[..hit.revision.len().min(16)],
                hit.modified_at.0
            );
            let flat: String = hit.text.chars().filter(|c| *c != '\n').collect();
            let excerpt: String = flat.chars().take(160).collect();
            println!("    {excerpt}");
        }
    }
    for (alias, reason) in &response.freshness.skipped_roots {
        println!("freshness: root '{alias}' skipped: {reason}");
    }
    Ok(())
}
