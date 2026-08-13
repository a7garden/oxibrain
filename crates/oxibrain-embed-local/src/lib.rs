//! oxibrain-embed-local — multilingual encoder behind `EmbeddingPort`
//! (ARCHITECTURE.md §8.2).
//!
//! Wraps `llama-cpp-2` with an embedding GGUF model (GTE-multilingual, BGE,
//! E5, etc.). Separate from `oxibrain-llm-local`: different model lifecycle,
//! and retrieval-only deployments need embeddings without inference.
//!
//! Embeddings are computed on a blocking thread (`spawn_blocking`); never on
//! the writer actor and never inside a transaction.

use oxibrain_ports::{BrainError, EmbeddingPort};
use std::path::Path;
use std::sync::Arc;

use llama_cpp_2::model::LlamaModel;

/// Options for opening a local embedder.
#[derive(Debug, Clone)]
pub struct LocalEmbedderOptions {
    /// GPU layer offload. `0` = CPU only. Default: all layers.
    pub n_gpu_layers: u32,
    /// Context size in tokens. Default: 512 (embeddings use short inputs).
    pub n_ctx: u32,
    /// Threads for prompt processing. Default: 4.
    pub n_threads: i32,
    /// Prefix prepended to every input (E5-style "query: "/"passage: ").
    /// Empty for models without prefix conventions (GTE, BGE without prefixes).
    pub prefix: String,
}

impl Default for LocalEmbedderOptions {
    fn default() -> Self {
        Self {
            n_gpu_layers: 1000,
            n_ctx: 512,
            n_threads: 4,
            prefix: String::new(),
        }
    }
}

/// A local embedding model behind the oxibrain ports.
pub struct LocalEmbedder {
    model: Arc<LlamaModel>,
    model_id: String,
    dim: usize,
    opts: LocalEmbedderOptions,
}

impl std::fmt::Debug for LocalEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalEmbedder")
            .field("model_id", &self.model_id)
            .field("dim", &self.dim)
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl LocalEmbedder {
    /// Load an embedding GGUF model from `path`.
    pub fn open(path: &Path, opts: LocalEmbedderOptions) -> Result<Self, BrainError> {
        let backend = llama_backend();
        let params = llama_cpp_2::model::params::LlamaModelParams::default()
            .with_n_gpu_layers(opts.n_gpu_layers);
        let model = LlamaModel::load_from_file(backend, path, &params).map_err(|e| {
            BrainError::Model(format!(
                "failed to load embedding model {}: {e}",
                path.display()
            ))
        })?;
        // Embedding output width — not the raw hidden width (n_embd_out).
        let dim = usize::try_from(model.n_embd_out()).expect("n_embd_out fits usize");
        let model_id = model.meta_val_str("general.name").unwrap_or_else(|_| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "local-embedder".to_string())
        });
        Ok(Self {
            model: Arc::new(model),
            model_id,
            dim,
            opts,
        })
    }

    /// Model identifier (from GGUF metadata or file name).
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Shared llama.cpp backend — initialized once per process.
fn llama_backend() -> &'static llama_cpp_2::llama_backend::LlamaBackend {
    use llama_cpp_2::llama_backend::LlamaBackend;
    use std::sync::OnceLock;
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    BACKEND.get_or_init(|| LlamaBackend::init().expect("llama.cpp backend init failed"))
}

// ─── EmbeddingPort ──────────────────────────────────────────────────────────

impl EmbeddingPort for LocalEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, BrainError> {
        let texts: Vec<String> = texts
            .iter()
            .map(|t| format!("{}{}", self.opts.prefix, t))
            .collect();
        embed_blocking(&self.model, &self.opts, &texts)
    }
}

fn embed_blocking(
    model: &LlamaModel,
    opts: &LocalEmbedderOptions,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, BrainError> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::AddBos;

    let backend = llama_backend();

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(
            std::num::NonZeroU32::new(opts.n_ctx).expect("n_ctx > 0"),
        ))
        .with_n_threads(opts.n_threads)
        .with_embeddings(true);
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| BrainError::Model(format!("embedding context creation: {e}")))?;

    // Process one text per decode. Multi-sequence batching fails on
    // embedding models with LLAMA_POOLING_TYPE_NONE (e.g. BGE-M3 GGUF);
    // single-sequence decode + embeddings_seq_ith works reliably.
    let mut out = Vec::with_capacity(texts.len());
    for (idx, t) in texts.iter().enumerate() {
        // AddBos::Always for BERT-family models: prepends [CLS], appends [SEP].
        let toks = model
            .str_to_token(t, AddBos::Always)
            .map_err(|e| BrainError::Model(format!("embedding tokenization: {e}")))?;
        if toks.is_empty() {
            continue;
        }
        let mut batch = LlamaBatch::new(toks.len(), 1);
        let last = toks.len() - 1;
        for (i, &token) in toks.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == last)
                .map_err(|e| BrainError::Model(format!("embedding batch add: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| BrainError::Model(format!("embedding decode text {idx}: {e}")))?;

        let emb = ctx
            .embeddings_seq_ith(0)
            .map_err(|e| BrainError::Model(format!("embedding extract text {idx}: {e}")))?;
        out.push(l2_normalize(emb));
    }
    Ok(out)
}

/// L2-normalize an embedding in place. Zero vectors stay zero.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}
