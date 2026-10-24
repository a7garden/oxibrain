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

const MIN_CONTEXT_TOKENS: u32 = 512;

/// Options for opening a local embedder.
#[derive(Debug, Clone)]
pub struct LocalEmbedderOptions {
    /// GPU layer offload. `0` = CPU only. Default: all layers.
    pub n_gpu_layers: u32,
    /// Maximum complete input sequence length in tokens. Default: 8192.
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
            n_ctx: 8192,
            n_threads: 4,
            prefix: String::new(),
        }
    }
}

/// A local embedding model behind the oxibrain ports.
pub struct LocalEmbedder {
    model: Arc<LlamaModel>,
    model_id: String,
    identity: String,
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
        validate_options(&opts)?;
        let initial_digest = oxibrain_digest(path)?;
        let backend = llama_backend()?;
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
        let digest = oxibrain_digest(path)?;
        if digest != initial_digest {
            return Err(BrainError::Busy(
                "embedding weights changed while loading".into(),
            ));
        }
        let identity = format!(
            "gguf:{digest}:dim:{dim}:ctx:{}:prefix:{}",
            opts.n_ctx,
            blake3::hash(opts.prefix.as_bytes()).to_hex()
        );
        Ok(Self {
            identity,
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
///
/// llama.cpp logs through a process-global C callback that defaults to
/// `fputs(…, stderr)`; left alone, every model load spams Metal device init,
/// model metadata dumps, `sched_reserve`, and compute-buffer notes. Unless
/// `OXIBRAIN_VERBOSE=1`, install the upstream disabled log sink before
/// `llama_backend_init` so init-time device logs are swallowed too. The sink
/// checks its disabled flag before touching `tracing`, so a subscriber
/// installed elsewhere in the process cannot resurrect the noise. When the
/// env var is set, the default callback is left in place: byte-identical
/// stderr passthrough, as before this hook existed.
fn llama_backend() -> Result<&'static llama_cpp_2::llama_backend::LlamaBackend, BrainError> {
    oxibrain_llama_runtime::backend()
}

// ─── EmbeddingPort ──────────────────────────────────────────────────────────

impl EmbeddingPort for LocalEmbedder {
    fn identity(&self) -> Option<String> {
        Some(self.identity.clone())
    }

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

    validate_options(opts)?;

    // Tokenize every complete sequence before constructing a context. Encoder
    // models cannot be micro-batched or split without changing their semantics.
    let mut tokenized = Vec::with_capacity(texts.len());
    for (idx, text) in texts.iter().enumerate() {
        let tokens = model
            .str_to_token(text, AddBos::Always)
            .map_err(|e| BrainError::Model(format!("embedding tokenization: {e}")))?;
        if tokens.is_empty() {
            return Err(BrainError::Invalid(format!(
                "embedding input {idx} tokenized to an empty sequence"
            )));
        }
        tokenized.push(tokens);
    }
    let model_n_ctx_train = model.n_ctx_train();
    let Some(plan) = context_size_plan(
        opts.n_ctx,
        model_n_ctx_train,
        tokenized.iter().map(Vec::len),
    )?
    else {
        return Ok(Vec::new());
    };

    let backend = llama_backend()?;

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(
            std::num::NonZeroU32::new(plan.n_ctx).expect("validated nonzero context size"),
        ))
        .with_n_batch(plan.n_batch)
        .with_n_ubatch(plan.n_ubatch)
        .with_n_threads(opts.n_threads)
        .with_embeddings(true);
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| BrainError::Model(format!("embedding context creation: {e}")))?;

    // Process one text per decode. Multi-sequence batching fails on
    // embedding models with LLAMA_POOLING_TYPE_NONE (e.g. BGE-M3 GGUF);
    // single-sequence decode + embeddings_seq_ith works reliably.
    let mut out = Vec::with_capacity(texts.len());
    for (idx, toks) in tokenized.iter().enumerate() {
        ctx.clear_kv_cache();
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

fn validate_options(opts: &LocalEmbedderOptions) -> Result<(), BrainError> {
    if opts.n_ctx == 0 {
        return Err(BrainError::Invalid(
            "embedding n_ctx must be greater than zero".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContextSizePlan {
    n_ctx: u32,
    n_batch: u32,
    n_ubatch: u32,
}

fn context_size_plan(
    n_ctx: u32,
    model_n_ctx_train: u32,
    token_counts: impl IntoIterator<Item = usize>,
) -> Result<Option<ContextSizePlan>, BrainError> {
    if n_ctx == 0 {
        return Err(BrainError::Invalid(
            "embedding n_ctx must be greater than zero".into(),
        ));
    }

    // A zero training-context value means the model metadata does not declare
    // a limit; in that case retain the caller's explicit cap. Otherwise honor
    // both the configured cap and the architecture's trained context size.
    let effective_limit = if model_n_ctx_train == 0 {
        n_ctx
    } else {
        n_ctx.min(model_n_ctx_train)
    };

    let mut max_tokens = 0_u32;
    for (index, count) in token_counts.into_iter().enumerate() {
        if count == 0 {
            return Err(BrainError::Invalid(format!(
                "embedding input {index} tokenized to an empty sequence"
            )));
        }
        let count = u32::try_from(count).map_err(|_| {
            BrainError::Invalid(format!(
                "embedding input {index} token count exceeds supported context size"
            ))
        })?;
        if count > i32::MAX as u32 {
            return Err(BrainError::Invalid(format!(
                "embedding input {index} token count exceeds supported batch size"
            )));
        }
        if count > effective_limit {
            return Err(BrainError::Invalid(format!(
                "embedding input {index} has {count} tokens, exceeding effective context limit {effective_limit} (configured n_ctx {n_ctx}, model n_ctx_train {model_n_ctx_train})"
            )));
        }
        max_tokens = max_tokens.max(count);
    }
    Ok((max_tokens > 0).then_some(ContextSizePlan {
        n_ctx: max_tokens.max(effective_limit.min(MIN_CONTEXT_TOKENS)),
        n_batch: max_tokens,
        n_ubatch: max_tokens,
    }))
}

/// L2-normalize an embedding in place. Zero vectors stay zero.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

fn oxibrain_digest(path: &Path) -> Result<String, BrainError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| BrainError::Model(format!("embedding weights: {e}")))?;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| BrainError::Model(format!("embedding digest: {e}")))?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(digest.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_context_before_loading_model() {
        let error = LocalEmbedder::open(
            Path::new("missing-model.gguf"),
            LocalEmbedderOptions {
                n_ctx: 0,
                ..LocalEmbedderOptions::default()
            },
        )
        .expect_err("zero context must fail before reading a model path");
        assert!(matches!(error, BrainError::Invalid(_)));
    }

    #[test]
    fn context_size_plan_uses_largest_complete_sequence() {
        assert_eq!(
            context_size_plan(8192, 8192, [24, 513, 400]).expect("valid size plan"),
            Some(ContextSizePlan {
                n_ctx: 513,
                n_batch: 513,
                n_ubatch: 513,
            })
        );
    }

    #[test]
    fn context_size_plan_accepts_exact_limit_and_rejects_over_limit() {
        assert_eq!(
            context_size_plan(513, 513, [513]).expect("valid size plan"),
            Some(ContextSizePlan {
                n_ctx: 513,
                n_batch: 513,
                n_ubatch: 513,
            })
        );
        let error =
            context_size_plan(512, 8192, [513]).expect_err("over-context input is rejected");
        assert!(
            matches!(error, BrainError::Invalid(message) if message.contains("effective context limit 512") && message.contains("model n_ctx_train 8192"))
        );
    }

    #[test]
    fn context_size_plan_honors_model_training_limit_without_rejecting_open_cap() {
        // A configured cap may exceed the model's positional embedding limit;
        // short inputs remain valid and allocate only within model support.
        assert_eq!(
            context_size_plan(8192, 512, [400]).expect("short input fits model limit"),
            Some(ContextSizePlan {
                n_ctx: 512,
                n_batch: 400,
                n_ubatch: 400,
            })
        );

        let error = context_size_plan(8192, 512, [513])
            .expect_err("sequence beyond model training context is rejected");
        assert!(matches!(
            error,
            BrainError::Invalid(message)
                if message.contains("effective context limit 512")
                    && message.contains("configured n_ctx 8192")
                    && message.contains("model n_ctx_train 512")
        ));
    }

    #[test]
    fn context_size_plan_uses_configured_limit_when_model_limit_is_unknown() {
        assert_eq!(
            context_size_plan(8192, 0, [513]).expect("unknown model limit uses configured cap"),
            Some(ContextSizePlan {
                n_ctx: 513,
                n_batch: 513,
                n_ubatch: 513,
            })
        );
    }

    #[test]
    fn context_size_plan_rejects_zero_and_empty_sequences() {
        assert!(matches!(
            context_size_plan(0, 0, []),
            Err(BrainError::Invalid(message)) if message.contains("n_ctx")
        ));
        assert!(matches!(
            context_size_plan(512, 512, [0]),
            Err(BrainError::Invalid(message)) if message.contains("empty sequence")
        ));
        assert_eq!(context_size_plan(512, 512, []).expect("empty plan"), None);
        assert_eq!(
            context_size_plan(8192, 8192, [32]).expect("valid size plan"),
            Some(ContextSizePlan {
                n_ctx: MIN_CONTEXT_TOKENS,
                n_batch: 32,
                n_ubatch: 32,
            })
        );
        assert_eq!(
            context_size_plan(256, 8192, [128]).expect("valid size plan"),
            Some(ContextSizePlan {
                n_ctx: 256,
                n_batch: 128,
                n_ubatch: 128,
            })
        );
    }
}
