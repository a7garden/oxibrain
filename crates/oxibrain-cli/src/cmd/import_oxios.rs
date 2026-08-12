//! `oxibrain import-oxios` — one-shot migration from an oxios-memory store.
//!
//! DESIGN §16.3: entries map to `SourceRef::AgentTrace`, trust `SemiTrusted`.
//! Original creation date is prepended to the content so extraction can
//! recover temporal facts — the content IS the source of truth (P2).

use oxibrain::{Brain, BrainConfig, SourceRef, TrustTier};
use oxibrain_connectors::read_oxios_memory;
use std::path::Path;

pub async fn run(dir: &Path, db: &Path, space: &str) -> anyhow::Result<()> {
    let entries = read_oxios_memory(db)?;
    let total = entries.len();

    if total == 0 {
        println!("no entries found in {}", db.display());
        return Ok(());
    }

    println!(
        "importing {total} entries from {} into space '{space}'…",
        db.display()
    );

    let brain = Brain::open(BrainConfig::at(dir)).await?;
    brain.ensure_space(space).await?;

    let mut ok = 0usize;
    let mut fail = 0usize;

    for entry in &entries {
        // Prepend original metadata so extraction can recover temporal context.
        let content = if let Some(summary) = &entry.summary {
            format!(
                "[Imported from oxios-memory. Type: {}. Created: {}. Source: {}. Summary: {}]\n\n{}",
                entry.memory_type, entry.created_at, entry.source, summary, entry.content,
            )
        } else {
            format!(
                "[Imported from oxios-memory. Type: {}. Created: {}. Source: {}]\n\n{}",
                entry.memory_type, entry.created_at, entry.source, entry.content,
            )
        };

        match brain
            .ingest(
                space,
                content,
                SourceRef::AgentTrace,
                TrustTier::SemiTrusted,
                "import-oxios/v1",
            )
            .await
        {
            Ok(_) => ok += 1,
            Err(e) => {
                eprintln!("  WARN: failed to import entry {}: {e}", entry.id);
                fail += 1;
            }
        }
    }

    println!("done: {ok} imported, {fail} failed (out of {total})");
    if ok > 0 {
        println!(
            "\nRun `oxibrain reextract --space {space}` to extract knowledge from imported entries."
        );
    }
    Ok(())
}
