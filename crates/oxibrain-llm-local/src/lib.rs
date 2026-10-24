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
    /// Maximum context size in tokens. The model's training context is also
    /// enforced for each request. Default: 16384.
    pub n_ctx: u32,
    pub n_threads: i32,
    /// Repetition penalty applied in the sampler chain. Default: 1.3. Reduces
    /// grammar-constrained greedy rambling on small local models.
    pub repetition_penalty: f32,
    /// Sampling temperature. Default: 0.4. Breaks the loops that pure
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
    /// Load a GGUF model from `path`. Returns `BrainError::Invalid` for an
    /// invalid context size and `BrainError::Model` on missing or corrupt
    /// weights — never panics.
    pub fn open(path: &Path, opts: LocalLlmOptions) -> Result<Self, BrainError> {
        validate_context_option(opts.n_ctx)?;
        let backend = llama_backend()?;
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
            .unwrap_or(usize::MAX)
    }
    fn count_exact(&self, text: &str) -> Result<Option<usize>, BrainError> {
        self.model
            .str_to_token(text, llama_cpp_2::model::AddBos::Never)
            .map(|t| Some(t.len()))
            .map_err(|e| BrainError::TokenizerUnavailable(e.to_string()))
    }
    fn quality(&self) -> oxibrain_ports::tokenizer::TokenizerQuality {
        oxibrain_ports::tokenizer::TokenizerQuality::Exact
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

    // Tokenize and validate the complete prompt before creating a context or
    // asking llama.cpp to decode it. No truncation is allowed here: it would
    // silently change the user's input and can split structured evidence.
    let tokens = model
        .str_to_token(&prompt, AddBos::Never)
        .map_err(|e| BrainError::Model(format!("tokenization: {e}")))?;
    let plan = plan_generation(
        opts.n_ctx,
        model.n_ctx_train(),
        tokens.len(),
        req.max_tokens,
    )?;

    let backend = llama_backend()?;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(Some(
            NonZeroU32::new(plan.context_tokens).expect("validated positive context"),
        ))
        // Decode the entire prompt as one batch. Set n_batch to the actual
        // token count so a long prompt cannot exceed llama.cpp's batch limit.
        .with_n_batch(plan.prompt_tokens as u32)
        .with_n_threads(opts.n_threads);
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| BrainError::Model(format!("context creation: {e}")))?;

    // Feed the prompt into a batch. The batch only ever holds the full prompt
    // (generation clears it to one token per step), so capacity follows the
    // preflighted prompt length.
    let mut batch = LlamaBatch::new(tokens.len(), 1);
    let last_index = plan.prompt_tokens - 1;
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
                LlamaSampler::penalties(model.n_vocab(), 64, opts.repetition_penalty, 0.0, 0.0),
                LlamaSampler::temp(opts.temperature),
                LlamaSampler::greedy(),
            ])
        }
        None => LlamaSampler::chain_simple([
            LlamaSampler::penalties(model.n_vocab(), 64, opts.repetition_penalty, 0.0, 0.0),
            LlamaSampler::temp(opts.temperature),
            LlamaSampler::greedy(),
        ]),
    };
    // Generation loop. n_cur tracks total position; n_decode tracks output.
    // The KV cache must hold prompt + output within n_ctx: without the
    // budget below, a long prompt pushes decode past the context and llama.cpp
    // fails the batch silently ("failed to find a memory slot"), truncating
    // the JSON mid-string with no error surfaced.
    let max_out = plan.max_output_tokens;
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

const OUTPUT_CONTEXT_RESERVE: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GenerationPlan {
    context_tokens: u32,
    prompt_tokens: i32,
    max_output_tokens: i32,
}

fn validate_context_option(n_ctx: u32) -> Result<(), BrainError> {
    if n_ctx == 0 {
        return Err(BrainError::Invalid(
            "local model n_ctx must be greater than zero".into(),
        ));
    }
    Ok(())
}

/// Validate context and output limits without allocating a native context.
/// A request's output is capped to the available room, matching the previous
/// behavior, while an overlong prompt or a context with no output room is an
/// explicit error rather than a native decode failure or silent empty result.
fn plan_generation(
    configured_context: u32,
    model_context: u32,
    prompt_tokens: usize,
    requested_output_tokens: u32,
) -> Result<GenerationPlan, BrainError> {
    validate_context_option(configured_context)?;

    if requested_output_tokens > i32::MAX as u32 {
        return Err(BrainError::Invalid(format!(
            "requested max_tokens {requested_output_tokens} exceeds the local decoder limit {}",
            i32::MAX
        )));
    }

    let context_tokens = if model_context == 0 {
        configured_context
    } else {
        configured_context.min(model_context)
    };
    if context_tokens > i32::MAX as u32 {
        return Err(BrainError::Invalid(format!(
            "effective context {context_tokens} exceeds the local decoder position limit {}",
            i32::MAX
        )));
    }

    let prompt_tokens = u32::try_from(prompt_tokens).map_err(|_| {
        BrainError::Invalid("tokenized prompt exceeds the local decoder limit".into())
    })?;
    if prompt_tokens == 0 {
        return Err(BrainError::Invalid(
            "tokenized prompt must contain at least one token".into(),
        ));
    }
    let prompt_tokens_i32 = i32::try_from(prompt_tokens).map_err(|_| {
        BrainError::Invalid("tokenized prompt exceeds the local decoder position limit".into())
    })?;

    let output_capacity = context_tokens
        .saturating_sub(prompt_tokens)
        .saturating_sub(OUTPUT_CONTEXT_RESERVE);
    if output_capacity == 0 {
        return Err(BrainError::Invalid(format!(
            "prompt uses {prompt_tokens} tokens and leaves no output capacity in effective context {context_tokens} (configured {configured_context}, model training limit {model_context}, reserve {OUTPUT_CONTEXT_RESERVE})"
        )));
    }

    let max_output_tokens = requested_output_tokens.min(output_capacity);
    // Both inputs are bounded by i32::MAX above, so the selected output cap
    // also fits the signed decoder loop counter.
    let max_output_tokens = i32::try_from(max_output_tokens)
        .expect("validated requested output and context fit local decoder limit");

    Ok(GenerationPlan {
        context_tokens,
        prompt_tokens: prompt_tokens_i32,
        max_output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        GenerationPlan, LocalLlm, LocalLlmOptions, plan_generation, validate_context_option,
    };
    use oxibrain_ports::BrainError;
    use std::path::Path;

    #[test]
    fn generation_plan_uses_model_context_and_caps_requested_output() {
        let plan = plan_generation(4096, 512, 400, 200).expect("usable capacity");

        assert_eq!(
            plan,
            GenerationPlan {
                context_tokens: 512,
                prompt_tokens: 400,
                max_output_tokens: 104,
            }
        );
    }

    #[test]
    fn generation_plan_keeps_requested_output_when_it_fits() {
        let plan = plan_generation(512, 1024, 100, 64).expect("usable capacity");
        assert_eq!(plan.context_tokens, 512);
        assert_eq!(plan.prompt_tokens, 100);
        assert_eq!(plan.max_output_tokens, 64);
    }

    #[test]
    fn generation_plan_accepts_a_complete_long_prompt_that_fits_context() {
        let plan = plan_generation(4096, 8192, 3000, 128).expect("long prompt fits");
        assert_eq!(plan.context_tokens, 4096);
        assert_eq!(plan.prompt_tokens, 3000);
        assert_eq!(plan.max_output_tokens, 128);
    }

    #[test]
    fn generation_plan_rejects_zero_or_overlong_prompt_before_context_creation() {
        assert!(matches!(
            plan_generation(0, 1024, 1, 8),
            Err(BrainError::Invalid(_))
        ));
        assert!(matches!(
            plan_generation(512, 1024, 0, 8),
            Err(BrainError::Invalid(_))
        ));
        assert!(matches!(
            plan_generation(512, 256, 257, 8),
            Err(BrainError::Invalid(_))
        ));
    }

    #[test]
    fn generation_plan_rejects_missing_output_capacity_and_unsafe_request_size() {
        assert!(matches!(
            plan_generation(32, 32, 24, 1),
            Err(BrainError::Invalid(_))
        ));
        assert!(matches!(
            plan_generation(128, 128, 16, u32::MAX),
            Err(BrainError::Invalid(_))
        ));
    }

    #[test]
    fn zero_context_is_rejected_without_loading_a_model() {
        let err = LocalLlm::open(
            Path::new("/does/not/exist.gguf"),
            LocalLlmOptions {
                n_ctx: 0,
                ..LocalLlmOptions::default()
            },
        )
        .expect_err("zero context must be rejected before model loading");
        assert!(matches!(err, BrainError::Invalid(_)));
        assert!(validate_context_option(0).is_err());
    }
}
