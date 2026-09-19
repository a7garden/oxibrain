//! Local embedding-model attachment (§8.2).
//!
//! `oxibrain-embed-local` ships a multilingual embedder, but no CLI or MCP
//! surface attached it to `Brain`: `admin index --embed` failed with
//! "no embedding port configured" and the dense retrieval channel was
//! unreachable from every shipped binary. This module attaches the
//! manifest's `embed`-role model (default: bge-m3) to a `Brain`.

use oxibrain::Brain;
use oxibrain::models::{ModelRole, load_manifest_at, model_dir};
use oxibrain_embed_local::{LocalEmbedder, LocalEmbedderOptions};
use std::sync::Arc;

/// Open the manifest's `embed`-role model when it is present on disk.
///
/// MLX-enabled builds skip the llama.cpp embedder: statically linking
/// llama.cpp and mlx-c in one binary corrupts llama.cpp's GGUF parsing
/// (segfault in `gguf_get_key`, verified on the bge-m3 loader — ADR-017).
/// Query surfaces degrade to the lexical/graph channels; GGUF-only builds
/// keep the dense channel.
fn open_embedder() -> Option<LocalEmbedder> {
    if cfg!(feature = "mlx") {
        tracing::warn!(
            "mlx build: the llama.cpp embedder is disabled (static-link \
             conflict with mlx-c, ADR-017); dense retrieval is unavailable"
        );
        return None;
    }
    let dir = model_dir();
    let manifest = load_manifest_at(&dir).ok()?;
    let entry = manifest.into_iter().find(|e| e.role == ModelRole::Embed)?;
    let path = dir.join(&entry.file);
    if !path.exists() {
        return None;
    }
    LocalEmbedder::open(&path, LocalEmbedderOptions::default()).ok()
}

/// Attach the local embedder when pulled; return the brain unchanged
/// otherwise. Lexical and graph channels keep working without it, so query
/// surfaces call this best-effort.
pub fn try_attach_local_embedder(brain: Brain) -> Brain {
    match open_embedder() {
        Some(emb) => brain.with_embedder(Arc::new(emb)),
        None => brain,
    }
}

/// Attach the local embedder or fail loudly. For callers that require the
/// dense channel (`admin index --embed`): silently skipping would report
/// "dense coverage: n/a" as if nothing were wrong.
pub fn require_local_embedder(brain: Brain) -> anyhow::Result<Brain> {
    match open_embedder() {
        Some(emb) => Ok(brain.with_embedder(Arc::new(emb))),
        None => Err(anyhow::anyhow!(
            "no embedding model pulled — run: oxibrain model pull"
        )),
    }
}
