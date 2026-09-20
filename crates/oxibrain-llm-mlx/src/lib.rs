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

// The MLX engine is Apple-only. On other targets this crate compiles to an
// empty surface so workspace builds and `--all-features` CI legs succeed on
// Linux; the CLI's `mlx` feature is likewise inert there.

#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_vendor = "apple")]
pub mod config;
#[cfg(target_vendor = "apple")]
pub mod qwen3;
#[cfg(target_vendor = "apple")]
pub mod weights;

#[cfg(target_vendor = "apple")]
pub use apple::LocalMxlLlm;
