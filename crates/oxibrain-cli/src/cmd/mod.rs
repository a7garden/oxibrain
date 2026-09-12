pub mod backup;
pub mod doctor;
pub mod embed;
pub mod entity_split;
pub mod eval;
pub mod export_cmd;
pub mod extract;
pub mod foundation;
pub mod gate;
pub mod import_cmd;
pub mod import_oxios;
pub mod index;
pub mod init;
pub mod llm;
pub mod migrate;
pub mod model;
pub mod predicate;
pub mod provision;
pub mod reextract;
pub mod reproject;
pub mod serve;
pub mod skill;
pub mod source_policy;
pub mod space_add;
pub mod space_remove;
pub mod spaces;
pub mod stats;
pub mod token;

use anyhow::Result;
use oxibrain::Brain;

/// Resolve a space name to its id WITHOUT creating it (spec §4.4).
/// Unknown space ⇒ error carrying the `oxibrain space add` hint.
pub async fn space_id(brain: &Brain, name: &str) -> Result<String> {
    match brain.lookup_space(name).await {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(anyhow::anyhow!(
            "space '{name}' not found — create it with: oxibrain space add {name}"
        )),
        Err(e) => Err(anyhow::anyhow!("lookup space: {e}")),
    }
}
