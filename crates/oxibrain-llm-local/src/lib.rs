//! oxibrain-llm-local — GGUF inference behind `LlmPort` (ARCHITECTURE.md §8.2).
//!
//! Wraps `llama-cpp-2` (llama.cpp) for local inference: Metal on
//! aarch64-apple-darwin, CPU everywhere. Grammar-constrained decoding via
//! GBNF (§9.4, D28) is supported and advertised in [`LlmCapabilities`].
//!
//! Generation runs on a blocking thread (`tokio::task::spawn_blocking`), never
//! on the writer actor and never inside a transaction (§9.2).

use oxibrain_ports::{
    BrainError, LlmCapabilities, LlmPort, LlmRequest, LlmResponse, TokenizerPort,
};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use llama_cpp_2::model::LlamaModel;

/// Options for opening a local model.
#[derive(Debug, Clone)]
pub struct LocalLlmOptions {
    /// GPU layer offload. `0` = CPU only (portability floor). Default: all layers.
    pub n_gpu_layers: u32,
    /// Context size in tokens. Default: 16384 — the extraction prompt plus a
    /// max_tokens=8192 response must both fit or llama.cpp truncates silently.
    pub n_ctx: u32,
    pub n_threads: i32,
    /// Repetition penalty applied in the sampler chain. Default: 1.1. Reduces
    /// grammar-constrained greedy rambling on small local models.
    pub repetition_penalty: f32,
    /// Sampling temperature. Default: 0.2. Breaks the loops that pure
    /// greedy produces under a permissive GBNF grammar.
    pub temperature: f32,
}

impl Default for LocalLlmOptions {
    fn default() -> Self {
        Self {
            n_gpu_layers: 1000,
            n_ctx: 16384,
            n_threads: 4,
            repetition_penalty: 1.3,
            temperature: 0.4,
        }
    }
}

/// A local GGUF language model behind the oxibrain ports.
///
/// Implements [`LlmPort`] (generation, constrained generation) and
/// [`TokenizerPort`] (exact token counts from the model's tokenizer).
pub struct LocalLlm {
    model: Arc<LlamaModel>,
    model_id: String,
    opts: LocalLlmOptions,
}

impl std::fmt::Debug for LocalLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalLlm")
            .field("model_id", &self.model_id)
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl LocalLlm {
    /// Load a GGUF model from `path`. Returns `BrainError::Model` on missing
    /// or corrupt weights — never panics.
    pub fn open(path: &Path, opts: LocalLlmOptions) -> Result<Self, BrainError> {
        let backend = llama_backend();
        let params = llama_cpp_2::model::params::LlamaModelParams::default()
            .with_n_gpu_layers(opts.n_gpu_layers);
        let model = LlamaModel::load_from_file(backend, path, &params).map_err(|e| {
            BrainError::Model(format!("failed to load model {}: {e}", path.display()))
        })?;
        // Model id from the GGUF metadata, falling back to the file stem.
        let model_id = model.meta_val_str("general.name").unwrap_or_else(|_| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "local-model".to_string())
        });
        Ok(Self {
            model: Arc::new(model),
            model_id,
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

// ─── LlmPort ────────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl LlmPort for LocalLlm {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        generate(&self.model, &self.opts, &req, None).await
    }

    async fn generate_constrained(
        &self,
        req: LlmRequest,
        grammar: &str,
    ) -> Result<LlmResponse, BrainError> {
        generate(&self.model, &self.opts, &req, Some(grammar)).await
    }

    fn capabilities(&self) -> LlmCapabilities {
        // Local GGUF path uses GBNF grammar-constrained decoding (§9.4, D28).
        LlmCapabilities {
            grammar: true,
            structured_output: false,
            tool_call: false,
            json_schema: false,
        }
    }
}

// ─── TokenizerPort ──────────────────────────────────────────────────────────

impl TokenizerPort for LocalLlm {
    fn count(&self, text: &str) -> usize {
        self.model
            .str_to_token(text, llama_cpp_2::model::AddBos::Never)
            .map(|t| t.len())
            .unwrap_or_else(|_| (text.chars().count() / 4).max(1))
    }

    fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
        if let Ok(tokens) = self
            .model
            .str_to_token(text, llama_cpp_2::model::AddBos::Never)
        {
            if tokens.len() <= max_tokens {
                return text.to_string();
            }
            // Rebuild the truncated string token by token (non-deprecated path).
            let mut out = String::new();
            let mut decoder = encoding_rs::UTF_8.new_decoder();
            for &t in &tokens[..max_tokens] {
                if let Ok(piece) = self.model.token_to_piece(t, &mut decoder, true, None) {
                    out.push_str(&piece);
                }
            }
            return out;
        }
        // Fallback: chars/4 heuristic.
        let max_chars = max_tokens.saturating_mul(4);
        text.chars().take(max_chars).collect()
    }

    fn id(&self) -> &str {
        &self.model_id
    }
}

// ─── RerankPort (cross-encoder, §11.4, 10.5) ──────────────────────────────

/// A prompt-based cross-encoder reranker. Wraps any [`LlmPort`] and scores
/// (query, item) pairs by asking the model to rate relevance 0.0–1.0.
/// One LLM call per item — simple, correct, sufficient for v1.
pub struct CrossEncoderReranker {
    llm: std::sync::Arc<dyn LlmPort>,
    model_id: String,
    max_tokens: u32,
}

impl CrossEncoderReranker {
    pub fn new(llm: std::sync::Arc<dyn LlmPort>, model_id: impl Into<String>) -> Self {
        Self {
            llm,
            model_id: model_id.into(),
            max_tokens: 8,
        }
    }
}

#[async_trait::async_trait]
impl oxibrain_ports::RerankPort for CrossEncoderReranker {
    async fn rerank(
        &self,
        query: &str,
        mut items: Vec<oxibrain_ports::RerankItem>,
    ) -> Result<Vec<oxibrain_ports::RerankItem>, BrainError> {
        for item in items.iter_mut() {
            let prompt = format!(
                "Rate the relevance of this document to the query on a scale of 0.0 to 1.0. \
                 Respond with only the number.\nQuery: {query}\nDocument: {text}",
                text = item.text
            );
            let req = LlmRequest {
                model: self.model_id.clone(),
                system: Some("You are a relevance scoring engine.".into()),
                prompt,
                json_schema: None,
                max_tokens: self.max_tokens,
            };
            let resp = self.llm.complete(req).await?;
            // Parse the score: take the first f64-looking substring.
            let score: f64 = resp
                .text
                .trim()
                .split(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                .find(|s| !s.is_empty())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            item.score = score;
        }
        items.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(items)
    }
}

// ─── Generation ─────────────────────────────────────────────────────────────

async fn generate(
    model: &Arc<LlamaModel>,
    opts: &LocalLlmOptions,
    req: &LlmRequest,
    grammar: Option<&str>,
) -> Result<LlmResponse, BrainError> {
    let model = Arc::clone(model);
    let opts = opts.clone();
    let req = req.clone();
    let grammar = grammar.map(str::to_string);

    tokio::task::spawn_blocking(move || generate_blocking(&model, &opts, &req, grammar.as_deref()))
        .await
        .map_err(|e| BrainError::Storage(format!("generation task join: {e}")))?
}

fn generate_blocking(
    model: &LlamaModel,
    opts: &LocalLlmOptions,
    req: &LlmRequest,
    grammar: Option<&str>,
) -> Result<LlmResponse, BrainError> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::AddBos;
    use llama_cpp_2::sampling::LlamaSampler;

    let backend = llama_backend();

    // Build the chat-formatted prompt. Qwen-style delimiters work for most
    // modern instruct models (Qwen, Llama 3, Mistral).
    let prompt = match &req.system {
        Some(sys) => format!(
            "<|im_start|>system\n{sys}<|im_end|>\n\
             <|im_start|>user\n{}<|im_end|>\n\
             <|im_start|>assistant\n",
            req.prompt
        ),
        None => format!(
            "<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            req.prompt
        ),
    };

    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(NonZeroU32::new(opts.n_ctx).expect("n_ctx > 0")))
        // The whole prompt is decoded in one batch; llama.cpp asserts
        // (aborts the process) when a decode exceeds the default n_batch
        // of 2048 tokens — long non-ASCII notes tokenize past it easily.
        .with_n_batch(opts.n_ctx)
        .with_n_threads(opts.n_threads);
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| BrainError::Model(format!("context creation: {e}")))?;

    let tokens = model
        .str_to_token(&prompt, AddBos::Never)
        .map_err(|e| BrainError::Model(format!("tokenization: {e}")))?;

    // Feed the prompt into a batch. The batch only ever holds the full prompt
    // (generation clears it to one token per step), so capacity follows the
    // prompt — the old clamp(…, 8192) cap made prompts longer than 8K tokens
    // impossible to feed at all.
    let batch_cap = tokens.len().max(1024) + 64;
    let mut batch = LlamaBatch::new(batch_cap, 1);
    let last_index = (tokens.len() - 1) as i32;
    for (i, token) in (0_i32..).zip(tokens.iter().copied()) {
        let is_last = i == last_index;
        batch
            .add(token, i, &[0], is_last)
            .map_err(|e| BrainError::Model(format!("batch add: {e}")))?;
    }

    ctx.decode(&mut batch)
        .map_err(|e| BrainError::Model(format!("prompt decode: {e}")))?;

    // Sampler chain: grammar + repetition penalty + temperature + greedy.
    // A small Qwen with a grammar and pure greedy rambles — it explores the
    // rule tree to its length limit without converging (§extraction tier-0
    // risk, M7 risk table). Repetition penalty + temp break the loops so
    let mut sampler = match grammar {
        Some(g) => {
            let gs = LlamaSampler::grammar(model, g, "root")
                .map_err(|e| BrainError::Model(format!("grammar init: {e}")))?;
            LlamaSampler::chain_simple([
                gs,
                LlamaSampler::penalties(64, opts.repetition_penalty, 0.0, 0.0),
                LlamaSampler::temp(opts.temperature),
                LlamaSampler::greedy(),
            ])
        }
        None => LlamaSampler::chain_simple([
            LlamaSampler::penalties(64, opts.repetition_penalty, 0.0, 0.0),
            LlamaSampler::temp(opts.temperature),
            LlamaSampler::greedy(),
        ]),
    };
    // Generation loop. n_cur tracks total position; n_decode tracks output.
    // The KV cache must hold prompt + output within n_ctx: without the
    // budget below, a long prompt pushes decode past the context and llama.cpp
    // fails the batch silently ("failed to find a memory slot"), truncating
    // the JSON mid-string with no error surfaced.
    let kv_budget = (opts.n_ctx as i32 - batch.n_tokens() - 8).max(0);
    let max_out = (req.max_tokens as i32).min(kv_budget);
    let mut n_cur = batch.n_tokens();
    let mut n_decode = 0;
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut output = String::new();

    while n_decode < max_out {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        // NOTE: sample() accepts internally; do NOT call accept() again.
        if model.is_eog_token(token) {
            break;
        }
        if let Ok(piece) = model.token_to_piece(token, &mut decoder, true, None) {
            output.push_str(&piece);
        }
        batch.clear();
        let _ = batch.add(token, n_cur, &[0], true);
        n_cur += 1;
        n_decode += 1;
        if ctx.decode(&mut batch).is_err() {
            break;
        }
    }

    Ok(LlmResponse {
        text: output,
        raw: serde_json::json!({
            "model": req.model,
            "tokens_generated": n_decode,
            "local": true,
        }),
    })
}
