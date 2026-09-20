#![cfg(target_vendor = "apple")]
#![allow(clippy::type_complexity, clippy::needless_range_loop)]

//! Graph parity: the engine's `next_token_logits` on the tiny model must
//! match an independent f64 CPU reference forward — attention (causal, GQA,
//! per-head QK norm, RoPE), MoE top-k routing, SwiGLU, norms, LM head —
//! implemented here with no MLX calls. Localizes graph bugs that
//! shape/finiteness checks cannot see.

mod common;
use common::build_tiny_model;

use oxibrain_llm_mlx::qwen3::Qwen3Model;
use oxibrain_llm_mlx::{config as xcfg, weights as xw};

const DIM: usize = 128;
const LAYERS: usize = 2;
const HEADS: usize = 4;
const KV_HEADS: usize = 2;
const HEAD_DIM: usize = 32;
const MOE_DIM: usize = 64;
const EXPERTS: usize = 4;
const TOP_K: usize = 2;
const VOCAB: usize = 64;
const GROUP: usize = 32;
const EPS: f64 = 1e-6;

// ── CPU model ─────────────────────────────────────────────────────────────

struct Q {
    packed: Vec<Vec<u32>>,
    scales: Vec<Vec<f32>>,
    biases: Vec<Vec<f32>>,
    bits: usize,
}

impl Q {
    fn dequant(&self) -> Vec<Vec<f32>> {
        let out = self.packed.len();
        let lanes = 32 / self.bits;
        let inn = self.packed[0].len() * lanes;
        let mask = (1u32 << self.bits) - 1;
        (0..out)
            .map(|r| {
                (0..inn)
                    .map(|c| {
                        let lane = (self.packed[r][c / lanes] >> ((c % lanes) * self.bits)) & mask;
                        let g = c / GROUP;
                        lane as f32 * self.scales[r][g] + self.biases[r][g]
                    })
                    .collect()
            })
            .collect()
    }

    fn clone_self(&self) -> Q {
        Q {
            packed: self.packed.clone(),
            scales: self.scales.clone(),
            biases: self.biases.clone(),
            bits: self.bits,
        }
    }

    fn apply(&self, x: &[f64]) -> Vec<f64> {
        let w = self.dequant();
        (0..w.len())
            .map(|r| x.iter().zip(&w[r]).map(|(a, b)| a * (*b as f64)).sum())
            .collect()
    }
}

struct LayerR {
    attn_norm: Vec<f32>,
    post_norm: Vec<f32>,
    q: Q,
    k: Q,
    v: Q,
    o: Q,
    q_norm: Vec<f32>,
    k_norm: Vec<f32>,
    gate: Vec<Vec<f32>>,
    experts: Vec<(Q, Q, Q)>,
}

fn rms64(x: &[f64], w: &[f32]) -> Vec<f64> {
    let mean = x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64;
    let inv = 1.0 / (mean + EPS).sqrt();
    x.iter()
        .zip(w)
        .map(|(v, g)| v * inv * (*g as f64))
        .collect()
}

fn silu(v: f64) -> f64 {
    v / (1.0 + (-v).exp())
}

fn load_cpu(dir: &std::path::Path) -> (Vec<Vec<f32>>, Vec<LayerR>, Vec<f32>, Vec<Vec<f32>>, f64) {
    let w = xw::load_weights(dir).unwrap();
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
    let theta = cfg["rope_theta"].as_f64().unwrap();

    let arr32 = |k: &str| -> Vec<f32> {
        w[k].as_dtype(mlx_rs::Dtype::Float32)
            .unwrap()
            .reshape(&[-1])
            .unwrap()
            .as_slice::<f32>()
            .to_vec()
    };
    // NOTE: scales row width must follow the true input width per tensor;
    // recompute from packed columns instead of assuming DIM.
    let quant_w = |k: &str, inn: usize| -> Q {
        let bits = if k.ends_with(".mlp.gate") { 8 } else { 4 };
        let packed_shape = w[&format!("{k}.weight")].shape().to_vec();
        let out = packed_shape[0] as usize;
        let packed_cols = packed_shape[1] as usize;
        let pq: Vec<u32> = w[&format!("{k}.weight")]
            .reshape(&[-1])
            .unwrap()
            .as_slice::<u32>()
            .to_vec();
        let sq = arr32(&format!("{k}.scales"));
        let bq = arr32(&format!("{k}.biases"));
        let groups = inn / GROUP;
        Q {
            packed: (0..out)
                .map(|r| pq[r * packed_cols..(r + 1) * packed_cols].to_vec())
                .collect(),
            scales: (0..out)
                .map(|r| sq[r * groups..(r + 1) * groups].to_vec())
                .collect(),
            biases: (0..out)
                .map(|r| bq[r * groups..(r + 1) * groups].to_vec())
                .collect(),
            bits,
        }
    };

    let embed = {
        let e = quant_w("model.embed_tokens", DIM);
        e.dequant()
    };
    let lm_head = {
        let l = quant_w("lm_head", DIM);
        l.dequant()
    };
    let mut layers = Vec::new();
    for i in 0..LAYERS {
        let p = format!("model.layers.{i}");
        let e_in = MOE_DIM;
        // Experts are saved stacked [E, out, packed]; slice per expert.
        let stacked = |name: &str, inn: usize| -> Vec<Q> {
            let key = format!("{p}.mlp.switch_mlp.{name}");
            let shape = w[&format!("{key}.weight")].shape().to_vec();
            assert_eq!(shape.len(), 3, "stacked expert tensor {key}");
            let (e_n, out, packed_cols) = (shape[0] as usize, shape[1] as usize, shape[2] as usize);
            let pq: Vec<u32> = w[&format!("{key}.weight")]
                .reshape(&[-1])
                .unwrap()
                .as_slice::<u32>()
                .to_vec();
            let sq = arr32(&format!("{key}.scales"));
            let bq = arr32(&format!("{key}.biases"));
            let groups = inn / GROUP;
            (0..e_n)
                .map(|e| Q {
                    packed: (0..out)
                        .map(|r| {
                            let base = (e * out + r) * packed_cols;
                            pq[base..base + packed_cols].to_vec()
                        })
                        .collect(),
                    scales: (0..out)
                        .map(|r| {
                            let base = (e * out + r) * groups;
                            sq[base..base + groups].to_vec()
                        })
                        .collect(),
                    biases: (0..out)
                        .map(|r| {
                            let base = (e * out + r) * groups;
                            bq[base..base + groups].to_vec()
                        })
                        .collect(),
                    bits: 4,
                })
                .collect()
        };
        let gs = stacked("gate_proj", DIM);
        let us = stacked("up_proj", DIM);
        let ds = stacked("down_proj", e_in);
        let mut experts = Vec::new();
        for e in 0..EXPERTS {
            experts.push((gs[e].clone_self(), us[e].clone_self(), ds[e].clone_self()));
        }
        layers.push(LayerR {
            attn_norm: arr32(&format!("{p}.input_layernorm.weight")),
            post_norm: arr32(&format!("{p}.post_attention_layernorm.weight")),
            q: quant_w(&format!("{p}.self_attn.q_proj"), DIM),
            k: quant_w(&format!("{p}.self_attn.k_proj"), DIM),
            v: quant_w(&format!("{p}.self_attn.v_proj"), DIM),
            o: quant_w(&format!("{p}.self_attn.o_proj"), HEADS * HEAD_DIM),
            q_norm: arr32(&format!("{p}.self_attn.q_norm.weight")),
            k_norm: arr32(&format!("{p}.self_attn.k_norm.weight")),
            gate: quant_w(&format!("{p}.mlp.gate"), DIM).dequant(),
            experts,
        });
    }
    let final_norm = arr32("model.norm.weight");
    (embed, layers, final_norm, lm_head, theta)
}

fn forward_cpu(
    embed: &[Vec<f32>],
    layers: &[LayerR],
    final_norm: &[f32],
    lm_head: &[Vec<f32>],
    theta: f64,
    tokens: &[u32],
) -> Vec<f64> {
    let seq = tokens.len();
    let mut h: Vec<Vec<f64>> = tokens
        .iter()
        .map(|t| embed[*t as usize].iter().map(|v| *v as f64).collect())
        .collect();

    for l in layers {
        let normed: Vec<Vec<f64>> = h.iter().map(|r| rms64(r, &l.attn_norm)).collect();
        let q_all: Vec<Vec<f64>> = normed.iter().map(|r| l.q.apply(r)).collect();
        let k_all: Vec<Vec<f64>> = normed.iter().map(|r| l.k.apply(r)).collect();
        let v_all: Vec<Vec<f64>> = normed.iter().map(|r| l.v.apply(r)).collect();

        let rotate = |_v: &[f64], d: usize, pos: usize| -> (f64, f64) {
            let freq = theta.powf(-(2.0 * d as f64) / HEAD_DIM as f64);
            let ang = freq * pos as f64;
            (ang.cos(), ang.sin())
        };

        let mut attn_out: Vec<Vec<f64>> = vec![vec![0.0; DIM]; seq];
        for t in 0..seq {
            let mut att = vec![0f64; HEADS * HEAD_DIM];
            for hdi in 0..HEADS {
                let kvh = hdi / (HEADS / KV_HEADS);
                let qh_raw = &q_all[t][hdi * HEAD_DIM..(hdi + 1) * HEAD_DIM];
                let qh_normed = rms64(qh_raw, &l.q_norm);
                let mut qh = qh_normed.clone();
                for d in 0..HEAD_DIM / 2 {
                    let (c, s) = rotate(&qh, d, t);
                    let a = qh[d];
                    let b = qh[d + HEAD_DIM / 2];
                    qh[d] = a * c - b * s;
                    qh[d + HEAD_DIM / 2] = a * s + b * c;
                }
                let mut scores = Vec::with_capacity(t + 1);
                for u in 0..=t {
                    let kh_raw = &k_all[u][kvh * HEAD_DIM..(kvh + 1) * HEAD_DIM];
                    let kh_normed = rms64(kh_raw, &l.k_norm);
                    let mut kh = kh_normed.clone();
                    for d in 0..HEAD_DIM / 2 {
                        let (c, s) = rotate(&kh, d, u);
                        let a = kh[d];
                        let b = kh[d + HEAD_DIM / 2];
                        kh[d] = a * c - b * s;
                        kh[d + HEAD_DIM / 2] = a * s + b * c;
                    }
                    let dot: f64 = (0..HEAD_DIM).map(|d| qh[d] * kh[d]).sum::<f64>()
                        / (HEAD_DIM as f64).sqrt();
                    scores.push(dot);
                }
                let maxv = scores.iter().cloned().fold(f64::MIN, f64::max);
                let exps: Vec<f64> = scores.iter().map(|s| (s - maxv).exp()).collect();
                let total: f64 = exps.iter().sum();
                for (u, e) in exps.iter().enumerate() {
                    let wgt = e / total;
                    for d in 0..HEAD_DIM {
                        att[hdi * HEAD_DIM + d] += wgt * v_all[u][kvh * HEAD_DIM + d];
                    }
                }
            }
            attn_out[t] = l.o.apply(&att);
        }
        for t in 0..seq {
            for d in 0..DIM {
                h[t][d] += attn_out[t][d];
            }
        }
        // MoE
        for t in 0..seq {
            let normed = rms64(&h[t], &l.post_norm);
            let logits: Vec<f64> = (0..EXPERTS)
                .map(|e| {
                    normed
                        .iter()
                        .zip(&l.gate[e])
                        .map(|(a, b)| a * (*b as f64))
                        .sum()
                })
                .collect();
            let mut idx: Vec<usize> = (0..EXPERTS).collect();
            idx.sort_by(|a, b| logits[*b].partial_cmp(&logits[*a]).unwrap());
            idx.truncate(TOP_K);
            let top: Vec<f64> = idx.iter().map(|i| logits[*i]).collect();
            let maxt = top.iter().cloned().fold(f64::MIN, f64::max);
            let exps: Vec<f64> = top.iter().map(|v| (v - maxt).exp()).collect();
            let total: f64 = exps.iter().sum();
            let mut out = vec![0f64; DIM];
            for (slot, e) in idx.iter().enumerate() {
                let wgt = exps[slot] / total;
                let (g, u, d) = &l.experts[*e];
                let gx = g.apply(&normed);
                let ux = u.apply(&normed);
                let mid: Vec<f64> = gx.iter().zip(&ux).map(|(a, b)| silu(*a) * b).collect();
                let ex = d.apply(&mid);
                for dd in 0..DIM {
                    out[dd] += wgt * ex[dd];
                }
            }
            for d in 0..DIM {
                h[t][d] += out[d];
            }
        }
    }
    let last = &h[seq - 1];
    let normed = rms64(last, final_norm);
    (0..VOCAB)
        .map(|v| {
            normed
                .iter()
                .zip(&lm_head[v])
                .map(|(a, b)| a * (*b as f64))
                .sum()
        })
        .collect()
}

#[test]
fn graph_parity_against_cpu_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    build_tiny_model(dir);

    let (embed, layers, final_norm, lm_head, theta) = load_cpu(dir);
    let cfg = xcfg::Qwen3Config::load(dir).unwrap();
    let w = xw::load_weights(dir).unwrap();
    let eos = xcfg::Qwen3Config::eos_token_ids(dir);
    let model = Qwen3Model::load(cfg, &w, eos).unwrap();

    let tokens: Vec<u32> = vec![2, 4, 5, 7];
    let mut cache = vec![None; model.num_layers()];
    let logits = model.next_token_logits(&tokens, &mut cache).unwrap();
    let engine: Vec<f32> = logits
        .as_dtype(mlx_rs::Dtype::Float32)
        .unwrap()
        .reshape(&[-1])
        .unwrap()
        .as_slice::<f32>()
        .to_vec();

    let reference = forward_cpu(&embed, &layers, &final_norm, &lm_head, theta, &tokens);

    let engine_argmax = engine
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    let ref_argmax = reference
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    let max_diff = engine
        .iter()
        .zip(&reference)
        .map(|(a, b)| (*a as f64 - b).abs())
        .fold(0.0, f64::max);
    println!("engine argmax={engine_argmax} ref argmax={ref_argmax} max_diff={max_diff}");
    assert_eq!(
        engine_argmax, ref_argmax,
        "argmax must match the CPU reference"
    );
    assert!(
        max_diff < 0.05,
        "logits must be numerically close to the reference: max diff {max_diff}"
    );
}
