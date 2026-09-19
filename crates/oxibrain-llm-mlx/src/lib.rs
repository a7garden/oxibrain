//! Native MLX inference for oxibrain (Apple Silicon, in-process).
//!
//! Loads MLX-format safetensors models (the `mlx-community` quants) directly
//! — no server, no Python, no external process. Structurally mirrors
//! `oxibrain-llm-local`: same ChatML prompt contract, greedy decoding for
//! deterministic extraction output, `TokenizerPort` from the model's own
//! `tokenizer.json`.
//!
//! All MLX arrays are created and evaluated on one dedicated worker thread:
//! MLX streams are thread-local, and an array may only be evaluated on the
//! thread that created it. The adapter hands requests to that thread and
//! awaits the reply.
//!
//! Unlike the GGUF path there is no GBNF grammar engine in MLX, so
//! capabilities advertise nothing and the extraction pipeline takes its
//! schema-and-repair branch with the validator as the gate (§9.4).

pub mod config;
pub mod qwen3;
pub mod weights;

use oxibrain_ports::{
    BrainError, LlmCapabilities, LlmPort, LlmRequest, LlmResponse, TokenizerPort,
};
use qwen3::Qwen3Model;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use tokenizers::Tokenizer;

const IM_START: &str = "<|im_start|>";
const IM_END: &str = "<|im_end|>";

struct Inner {
    model: Qwen3Model,
    tokenizer: Tokenizer,
    dir: PathBuf,
    n_ctx: i32,
}

enum Command {
    Generate {
        prompt: String,
        max_tokens: u32,
        reply: mpsc::Sender<Result<LlmResponse, BrainError>>,
    },
}

/// In-process MLX adapter over an MLX-format model directory. The MLX model
/// lives on a dedicated worker thread; `TokenizerPort` uses the model's own
/// tokenizer on the caller's side (the `tokenizers` crate is thread-safe).
pub struct LocalMxlLlm {
    tx: mpsc::Sender<Command>,
    tokenizer: Arc<Tokenizer>,
    dir: PathBuf,
    model_id: String,
}

impl LocalMxlLlm {
    /// Load a model by directory path, HF-cache repo id
    /// (`mlx-community/Qwen3-…-4bit`), or `OXIBRAIN_MLX_HOME` name.
    pub fn load(spec: &str, n_ctx: usize) -> Result<Self, BrainError> {
        let err = |msg: String| BrainError::Config(format!("MLX model `{spec}`: {msg}"));
        let dir = weights::resolve_model_dir(spec).map_err(err)?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| err(format!("load tokenizer.json: {e}")))?;

        let (tx, rx) = mpsc::channel::<Command>();
        let worker_dir = dir.clone();
        let n_ctx = n_ctx.max(1024) as i32;
        let spec_owned = spec.to_string();
        std::thread::Builder::new()
            .name(format!("oxibrain-mlx-{spec_owned}"))
            .spawn(move || LocalMxlLlm::worker(worker_dir, rx, n_ctx))
            .map_err(|e| err(format!("spawn MLX worker: {e}")))?;

        Ok(Self {
            tx,
            tokenizer: Arc::new(tokenizer),
            dir,
            model_id: spec_owned,
        })
    }

    /// The resolved model directory (provenance for logs).
    pub fn model_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Worker-loop result reporting: model load errors after spawn surface
    /// on the first request so `load` stays non-blocking on 17 GB reads.
    fn worker(dir: PathBuf, rx: mpsc::Receiver<Command>, n_ctx: i32) {
        let mut inner: Option<Inner> = None;
        let mut load_error: Option<String> = None;
        while let Ok(cmd) = rx.recv() {
            if inner.is_none() && load_error.is_none() {
                match load_inner(&dir, n_ctx) {
                    Ok(v) => inner = Some(v),
                    Err(e) => load_error = Some(e),
                }
            }
            let Command::Generate { prompt, max_tokens, reply } = cmd;
            let result = match &mut inner {
                Some(g) => generate(g, prompt, max_tokens),
                None => Err(BrainError::Config(format!(
                    "MLX worker failed to load the model: {}",
                    load_error.as_deref().unwrap_or("unknown")
                ))),
            };
            if reply.send(result).is_err() {
                return; // caller gone — stop the worker
            }
        }
    }
}

fn load_inner(dir: &std::path::Path, n_ctx: i32) -> Result<Inner, String> {
    let cfg = config::Qwen3Config::load(dir)?;
    let eos = config::Qwen3Config::eos_token_ids(dir);
    let w = weights::load_weights(dir)?;
    let model = Qwen3Model::load(cfg, &w, eos)?;
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| format!("load tokenizer.json: {e}"))?;
    Ok(Inner { model, tokenizer, dir: dir.to_path_buf(), n_ctx })
}

/// ChatML prompt, byte-identical to the `oxibrain-llm-local` contract so
/// extraction prompts behave the same on both engines.
fn chatml(system: Option<&str>, prompt: &str) -> String {
    match system {
        Some(sys) => format!(
            "{IM_START}system\n{sys}{IM_END}\n{IM_START}user\n{prompt}{IM_END}\n{IM_START}assistant\n"
        ),
        None => format!("{IM_START}user\n{prompt}{IM_END}\n{IM_START}assistant\n"),
    }
}

fn encode(tokenizer: &Tokenizer, text: &str) -> Result<Vec<u32>, BrainError> {
    tokenizer
        .encode(text, false)
        .map(|e| e.get_ids().to_vec())
        .map_err(|e| BrainError::Provider {
            retryable: false,
            message: format!("tokenize: {e}"),
        })
}

fn mlx_err(e: mlx_rs::error::Exception) -> BrainError {
    BrainError::Provider {
        retryable: false,
        message: format!("MLX: {e}"),
    }
}

/// Greedy generation on the worker thread.
fn generate(g: &mut Inner, prompt: String, max_tokens: u32) -> Result<LlmResponse, BrainError> {
    let _ = &g.dir;
    let mut tokens = encode(&g.tokenizer, &prompt)?;
    let max_new = i32::try_from(max_tokens).unwrap_or(i32::MAX / 2);
    let budget = (g.n_ctx - max_new.min(g.n_ctx / 2)).max(1);
    if tokens.len() as i32 > budget {
        let over = tokens.len() as i32 - budget;
        tokens.drain(..over as usize);
    }
    let prompt_tokens = tokens.len() as u32;

    let mut cache: Vec<Option<(mlx_rs::Array, mlx_rs::Array)>> = vec![None; g.model.num_layers()];
    let mut logits = g
        .model
        .next_token_logits(&tokens, &mut cache)
        .map_err(mlx_err)?;
    let mut generated: Vec<u32> = Vec::new();
    loop {
        if generated.len() as i32 >= max_new {
            break;
        }
        let next = Qwen3Model::argmax_token(&logits).map_err(mlx_err)?;
        if g.model.eos_token_ids().contains(&next) {
            break;
        }
        generated.push(next);
        logits = g
            .model
            .next_token_logits(&[next], &mut cache)
            .map_err(mlx_err)?;
    }
    let text = g
        .tokenizer
        .decode(&generated, true)
        .map_err(|e| BrainError::Provider {
            retryable: false,
            message: format!("detokenize: {e}"),
        })?;
    Ok(LlmResponse {
        text,
        raw: serde_json::json!({
            "engine": "mlx",
            "completion_tokens": generated.len(),
            "prompt_tokens": prompt_tokens,
        }),
    })
}

#[async_trait::async_trait]
impl LlmPort for LocalMxlLlm {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        let prompt = chatml(req.system.as_deref(), &req.prompt);
        let tx = self.tx.clone();
        let model_id = self.model_id.clone();
        tokio::task::spawn_blocking(move || {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(Command::Generate {
                prompt,
                max_tokens: req.max_tokens,
                reply: reply_tx,
            })
            .map_err(|_| BrainError::Provider {
                retryable: false,
                message: "MLX worker stopped".into(),
            })?;
            let mut response = reply_rx
                .recv()
                .map_err(|_| BrainError::Provider {
                    retryable: false,
                    message: "MLX worker dropped the request".into(),
                })
                .and_then(|r| r)?;
            if let Some(obj) = response.raw.as_object_mut() {
                obj.insert("model".into(), serde_json::json!(model_id));
            }
            Ok(response)
        })
        .await
        .map_err(|e| BrainError::Provider {
            retryable: false,
            message: format!("MLX task join: {e}"),
        })?
    }

    fn capabilities(&self) -> LlmCapabilities {
        // No GBNF in MLX: schema-and-repair + validator is the contract
        // (§9.4 preference order records the mechanism honestly).
        LlmCapabilities {
            grammar: false,
            structured_output: false,
            tool_call: false,
            json_schema: false,
        }
    }
}

impl TokenizerPort for LocalMxlLlm {
    fn count(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.get_ids().len().max(1))
            .unwrap_or(1)
    }

    fn truncate_to(&self, text: &str, max_tokens: usize) -> String {
        let Ok(enc) = self.tokenizer.encode(text, false) else {
            return text.to_string();
        };
        let ids = enc.get_ids();
        if ids.len() <= max_tokens {
            return text.to_string();
        }
        self.tokenizer
            .decode(&ids[..max_tokens], false)
            .unwrap_or_else(|_| text.to_string())
    }

    fn id(&self) -> &str {
        "mlx-qwen3"
    }
}
