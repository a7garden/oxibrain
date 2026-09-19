//! Shared tiny-model builder for the MLX engine tests.

use mlx_rs::ops;
use serde_json::json;
use tokenizers::models::wordlevel::{WordLevel, WordLevelBuilder};
use tokenizers::{AddedToken, Tokenizer};
use ahash::AHashMap;
use oxibrain_llm_mlx::weights::model_fingerprint;
use std::path::Path;


// Tiny geometry: everything divisible by group_size 32.
pub const DIM: usize = 128;
pub const LAYERS: usize = 2;
pub const HEADS: usize = 4;
pub const KV_HEADS: usize = 2;
pub const HEAD_DIM: usize = 32;
pub const MOE_DIM: usize = 64;
pub const EXPERTS: usize = 4;
pub const TOP_K: usize = 2;
pub const VOCAB: usize = 64;
pub const GROUP: i32 = 32;

/// Deterministic pseudo-random f32 in (-0.05, 0.05) — an LCG keeps every
/// run identical without pulling a RNG dependency.
fn lcg_values(seed: u64, n: usize) -> Vec<f32> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / u32::MAX as f32 - 0.5) * 0.1
        })
        .collect()
}

fn quantized(
    out: usize,
    inn: usize,
    seed: u64,
    bits: i32,
) -> mlx_rs::error::Result<(mlx_rs::Array, mlx_rs::Array, mlx_rs::Array)> {
    let dense = mlx_rs::Array::from_slice(&lcg_values(seed, out * inn), &[out as i32, inn as i32]);
    ops::quantize(&dense, Some(GROUP), Some(bits))
}

/// Build the full mlx-community-style model directory.
pub fn build_tiny_model(dir: &Path) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let mut tensors: Vec<(String, mlx_rs::Array)> = Vec::new();
    let mut seed = 1u64;

    let embed = mlx_rs::Array::from_slice(&lcg_values(seed, VOCAB * DIM), &[VOCAB as i32, DIM as i32]);
    let (e_q, e_s, e_b) = ops::quantize(&embed, Some(GROUP), Some(4)).unwrap();
    tensors.push(("model.embed_tokens.weight".into(), e_q));
    tensors.push(("model.embed_tokens.scales".into(), e_s));
    tensors.push(("model.embed_tokens.biases".into(), e_b));
    seed += 1;

    let lm = mlx_rs::Array::from_slice(&lcg_values(seed, VOCAB * DIM), &[VOCAB as i32, DIM as i32]);
    let (l_q, l_s, l_b) = ops::quantize(&lm, Some(GROUP), Some(4)).unwrap();
    tensors.push(("lm_head.weight".into(), l_q));
    tensors.push(("lm_head.scales".into(), l_s));
    tensors.push(("lm_head.biases".into(), l_b));
    seed += 1;

    for l in 0..LAYERS {
        let p = format!("model.layers.{l}");
        for name in ["q_proj", "k_proj", "v_proj", "o_proj"] {
            let out = match name {
                "q_proj" => HEADS * HEAD_DIM,
                "k_proj" | "v_proj" => KV_HEADS * HEAD_DIM,
                _ => DIM,
            };
            let inn = if name == "o_proj" { HEADS * HEAD_DIM } else { DIM };
            let (q, s, b) = quantized(out, inn, seed, 4).unwrap();
            seed += 1;
            tensors.push((format!("{p}.self_attn.{name}.weight"), q));
            tensors.push((format!("{p}.self_attn.{name}.scales"), s));
            tensors.push((format!("{p}.self_attn.{name}.biases"), b));
        }
        tensors.push((
            format!("{p}.self_attn.q_norm.weight"),
            mlx_rs::Array::from_slice(&lcg_values(seed, HEAD_DIM), &[HEAD_DIM as i32]),
        ));
        seed += 1;
        tensors.push((
            format!("{p}.self_attn.k_norm.weight"),
            mlx_rs::Array::from_slice(&lcg_values(seed, HEAD_DIM), &[HEAD_DIM as i32]),
        ));
        seed += 1;
        // Router gate at 8 bits (per-key override path).
        let (gq, gs, gb) = quantized(EXPERTS, DIM, seed, 8).unwrap();
        seed += 1;
        tensors.push((format!("{p}.mlp.gate.weight"), gq));
        tensors.push((format!("{p}.mlp.gate.scales"), gs));
        tensors.push((format!("{p}.mlp.gate.biases"), gb));
        // Real mlx-community layout: experts stacked into one
        // `[E, out, …]` tensor per projection.
        for name in ["gate_proj", "up_proj", "down_proj"] {
            let (out, inn) = match name {
                "gate_proj" | "up_proj" => (MOE_DIM, DIM),
                _ => (DIM, MOE_DIM),
            };
            let mut qs = Vec::new();
            let mut ss = Vec::new();
            let mut bs = Vec::new();
            for _ in 0..EXPERTS {
                let (q, s, b) = quantized(out, inn, seed, 4).unwrap();
                seed += 1;
                qs.push(q);
                ss.push(s);
                bs.push(b);
            }
            let stack = |parts: Vec<mlx_rs::Array>, out: usize| -> mlx_rs::Array {
                let rows: usize = parts[0].shape().iter().map(|x| *x as usize).product();
                mlx_rs::ops::concatenate(&parts.iter().collect::<Vec<_>>(), 0)
                    .unwrap()
                    .reshape(&[(EXPERTS * rows) as i32])
                    .unwrap()
                    .reshape(&[EXPERTS as i32, out as i32, (rows / out) as i32])
                    .unwrap()
            };
            let rows2 = |parts: &Vec<mlx_rs::Array>| {
                parts[0].shape()[0] as usize
            };
            let _ = rows2;
            tensors.push((format!("{p}.mlp.switch_mlp.{name}.weight"), stack(qs, out)));
            tensors.push((format!("{p}.mlp.switch_mlp.{name}.scales"), stack(ss, out)));
            tensors.push((format!("{p}.mlp.switch_mlp.{name}.biases"), stack(bs, out)));
        }
        tensors.push((
            format!("{p}.input_layernorm.weight"),
            mlx_rs::Array::from_slice(&lcg_values(seed, DIM), &[DIM as i32]),
        ));
        seed += 1;
        tensors.push((
            format!("{p}.post_attention_layernorm.weight"),
            mlx_rs::Array::from_slice(&lcg_values(seed, DIM), &[DIM as i32]),
        ));
        seed += 1;
    }
    tensors.push((
        "model.norm.weight".into(),
        mlx_rs::Array::from_slice(&lcg_values(seed, DIM), &[DIM as i32]),
    ));

    let shard = dir.join("model.safetensors");
    mlx_rs::Array::save_safetensors(tensors, None, &shard).unwrap();

    let mut quant = json!({"group_size": GROUP, "bits": 4});
    for l in 0..LAYERS {
        quant[format!("model.layers.{l}.mlp.gate")] = json!({"group_size": GROUP, "bits": 8});
    }
    let config = json!({
        "architectures": ["Qwen3MoeForCausalLM"],
        "hidden_size": DIM,
        "num_hidden_layers": LAYERS,
        "num_attention_heads": HEADS,
        "num_key_value_heads": KV_HEADS,
        "head_dim": HEAD_DIM,
        "intermediate_size": MOE_DIM,
        "moe_intermediate_size": MOE_DIM,
        "num_experts": EXPERTS,
        "num_experts_per_tok": TOP_K,
        "decoder_sparse_step": 1,
        "rms_norm_eps": 1e-6,
        "rope_theta": 10000.0,
        "tie_word_embeddings": false,
        "vocab_size": VOCAB,
        "quantization": quant,
    });
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    std::fs::write(
        dir.join("generation_config.json"),
        json!({"eos_token_id": [5, 6]}).to_string(),
    )
    .unwrap();

    // Tiny WordLevel tokenizer: ids 0..3 specials/structure, 4.. words.
    let mut vocab: AHashMap<String, u32> = AHashMap::new();
    for (i, tok) in ["<pad>", "<unk>", "<|im_start|>", "<|im_end|>"]
        .iter()
        .enumerate()
    {
        vocab.insert(tok.to_string(), i as u32);
    }
    let mut next = 4u32;
    let mut add = |w: &str, vocab: &mut AHashMap<String, u32>| {
        vocab.insert(w.to_string(), next);
        next += 1;
    };
    for w in ["system", "user", "assistant", "hello", "world", "foo", "bar", "a", "b"] {
        add(w, &mut vocab);
    }
    // ids 5 and 6 are "hello" and "world" — also the eos list; the adapter
    // must stop when the model emits one of them.
    let wl: WordLevel = WordLevelBuilder::new()
        .vocab(vocab)
        .unk_token("<unk>".to_string())
        .build()
        .unwrap();
    let mut tok = Tokenizer::new(wl);
    let specials: Vec<AddedToken> = ["<pad>", "<unk>", "<|im_start|>", "<|im_end|>"]
        .iter()
        .map(|t| AddedToken::from(*t, true))
        .collect();
    tok.add_special_tokens(&specials);
    tok.save(dir.join("tokenizer.json"), true).unwrap();

    let fp = model_fingerprint(dir).unwrap();
    (shard.display().to_string(), fp)
}

