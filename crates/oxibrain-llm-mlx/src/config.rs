//! HF `config.json` parsing for the MLX engine. Scoped to the Qwen3 family
//! (dense `Qwen3ForCausalLM` and MoE `Qwen3MoeForCausalLM`) — the family the
//! default model set targets. Per-key quantization overrides (e.g. 8-bit
//! router gates inside a 4-bit model) are honoured because `quantized_matmul`
//! needs the true `(group_size, bits)` of every tensor.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantParams {
    pub group_size: i32,
    pub bits: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Qwen3Config {
    #[serde(default)]
    pub architectures: Vec<String>,
    pub hidden_size: i32,
    pub num_hidden_layers: i32,
    pub num_attention_heads: i32,
    pub num_key_value_heads: i32,
    pub head_dim: i32,
    pub intermediate_size: i32,
    #[serde(default)]
    pub moe_intermediate_size: i32,
    #[serde(default)]
    pub num_experts: i32,
    #[serde(default)]
    pub num_experts_per_tok: i32,
    #[serde(default)]
    pub decoder_sparse_step: i32,
    pub rms_norm_eps: f32,
    #[serde(default = "default_rope_theta")]
    pub rope_theta: f64,
    #[serde(default)]
    pub tie_word_embeddings: bool,
    pub vocab_size: i32,
    #[serde(default)]
    pub quantization: Option<RawQuantization>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawQuantization {
    pub group_size: i32,
    pub bits: i32,
    /// Per-tensor overrides keyed by weight path (e.g.
    /// `model.layers.0.mlp.gate` with 8-bit inside a 4-bit model).
    #[serde(flatten)]
    pub overrides: HashMap<String, RawQuantOverride>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawQuantOverride {
    pub group_size: i32,
    pub bits: i32,
}

fn default_rope_theta() -> f64 {
    1_000_000.0
}

impl Qwen3Config {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(dir.join("config.json"))
            .map_err(|e| format!("read {}: {e}", dir.join("config.json").display()))?;
        serde_json::from_str(&text).map_err(|e| format!("parse config.json: {e}"))
    }

    pub fn is_moe(&self) -> bool {
        self.num_experts > 1
            || self
                .architectures
                .iter()
                .any(|a| a.contains("MoeForCausalLM"))
    }

    /// Default quantization when the model ships quantized weights.
    pub fn default_quant(&self) -> Option<QuantParams> {
        self.quantization.as_ref().map(|q| QuantParams {
            group_size: q.group_size,
            bits: q.bits,
        })
    }

    /// Effective quantization for a weight path. mlx-community quantization
    /// configs key overrides by the full weight path (including the
    /// `model.` prefix); the stripped form is accepted as a fallback.
    pub fn quant_for(&self, key: &str) -> Option<QuantParams> {
        let q = self.quantization.as_ref()?;
        if let Some(o) = q.overrides.get(key) {
            return Some(QuantParams {
                group_size: o.group_size,
                bits: o.bits,
            });
        }
        let strip = key.strip_prefix("model.").unwrap_or(key);
        if let Some(o) = q.overrides.get(strip) {
            return Some(QuantParams {
                group_size: o.group_size,
                bits: o.bits,
            });
        }
        Some(QuantParams {
            group_size: q.group_size,
            bits: q.bits,
        })
    }

    /// End-of-sequence token ids from `generation_config.json` (may be a
    /// list). Falls back to the Qwen3 chat terminators.
    pub fn eos_token_ids(dir: &Path) -> Vec<u32> {
        let ids = std::fs::read_to_string(dir.join("generation_config.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| match v.get("eos_token_id") {
                Some(serde_json::Value::Array(a)) => a
                    .iter()
                    .filter_map(|x| x.as_u64())
                    .map(|x| x as u32)
                    .collect::<Vec<u32>>()
                    .into(),
                Some(serde_json::Value::Number(n)) => n.as_u64().map(|x| vec![x as u32]),
                _ => None,
            });
        ids.filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec![151_645, 151_643])
    }
}
