//! Qwen3 forward pass over raw MLX arrays (dense + MoE), mirroring mlx-lm's
//! reference implementation: GQA attention with per-head QK RMSNorm and RoPE,
//! SwiGLU experts, top-k softmax routing. Weights stay quantized on the GPU;
//! only the embedding table, LM head, and router gates are dequantized at
//! load (one-time, ~1 GB for the 30B-A3B) because their access pattern
//! (row gather / full-vocab matmul) is not a grouped-QMM shape.

use crate::config::{QuantParams, Qwen3Config};
use mlx_rs::fast;
use mlx_rs::nn;
use mlx_rs::ops;
use mlx_rs::ops::indexing::{argmax_axis, take_along_axis, IndexOp};
use mlx_rs::{Array, Dtype};
use std::collections::HashMap;

type W = HashMap<String, Array>;
type MlxResult<T> = mlx_rs::error::Result<T>;

// ── linear layers ─────────────────────────────────────────────────────────

/// A `quantized_matmul`-backed linear: `y = x @ deq(W)^T`.
pub(crate) struct QLinear {
    qweight: Array,
    scales: Array,
    biases: Array,
    group_size: i32,
    bits: i32,
}

impl QLinear {
    fn new(w: &W, key: &str, q: QuantParams) -> Result<Self, String> {
        // mlx-community packs quantized weights under `.weight` (with
        // sibling `.scales`/`.biases`); accept a `.qweight` spelling too.
        let packed_key = if w.contains_key(&format!("{key}.scales")) {
            format!("{key}.weight")
        } else {
            format!("{key}.qweight")
        };
        let (Some(qw), Some(s), Some(b)) = (
            w.get(&packed_key),
            w.get(&format!("{key}.scales")),
            w.get(&format!("{key}.biases")),
        ) else {
            return Err(format!("missing quantized tensors for `{key}`"));
        };
        Ok(Self {
            qweight: qw.clone(),
            scales: s.clone(),
            biases: b.clone(),
            group_size: q.group_size,
            bits: q.bits,
        })
    }

    fn forward(&self, x: &Array) -> MlxResult<Array> {
        ops::quantized_matmul(
            x,
            &self.qweight,
            &self.scales,
            &self.biases,
            Some(true),
            Some(self.group_size),
            Some(self.bits),
        )
    }
}

/// An fp16/bf16 linear over a dequantized `[out, in]` weight.
pub(crate) struct Linear {
    w: Array,
}

impl Linear {
    fn forward(&self, x: &Array) -> MlxResult<Array> {
        x.matmul(&self.w.transpose_axes(&[1, 0])?)
    }
}

fn float_buf(a: &Array) -> Result<Vec<f32>, String> {
    match a.dtype() {
        Dtype::Float32 => Ok(a.as_slice::<f32>().to_vec()),
        Dtype::Float16 => Ok(a
            .as_slice::<half::f16>()
            .iter()
            .map(|v| v.to_f32())
            .collect()),
        Dtype::Bfloat16 => Ok(a
            .as_slice::<half::bf16>()
            .iter()
            .map(|v| v.to_f32())
            .collect()),
        other => Err(format!("unsupported scale dtype {other:?}")),
    }
}

/// Dequantize packed mlx-community weights to a dense `[out, in]` array on
/// the CPU. MLX affine quantization packs unsigned `bits`-wide lanes
/// LSB-first into u32 words; `value = lane * scale + group_bias` with one
/// scale/bias pair per `group_size` input elements. `ops::dequantize` is not
/// used: it returns wrong values for non-square shapes (verified against
/// `quantized_matmul` on square weights, where the orientation bug hides).
fn dequantize_to_dense(w: &W, key: &str, q: QuantParams) -> Result<Array, String> {
    let packed_key = if w.contains_key(&format!("{key}.scales")) {
        format!("{key}.weight")
    } else {
        format!("{key}.qweight")
    };
    let qw = w
        .get(&packed_key)
        .ok_or_else(|| format!("missing `{packed_key}`"))?;
    let scales = w
        .get(&format!("{key}.scales"))
        .ok_or_else(|| format!("missing `{key}.scales`"))?;
    let biases = w
        .get(&format!("{key}.biases"))
        .ok_or_else(|| format!("missing `{key}.biases`"))?;
    dequantize_cpu(qw, scales, biases, q)
}

fn dequantize_cpu(qw: &Array, scales: &Array, biases: &Array, q: QuantParams) -> Result<Array, String> {
    let out = qw.shape()[0] as usize;
    let packed = qw.shape()[1] as usize;
    let lanes = 32 / q.bits as usize;
    let inn = packed * lanes;
    if !inn.is_multiple_of(q.group_size as usize) {
        return Err(format!(
            "group_size {} does not divide input width {inn}",
            q.group_size
        ));
    }
    let groups = inn / q.group_size as usize;
    let qbuf = qw.as_slice::<u32>().to_vec();
    let sbuf = float_buf(scales)?;
    let bbuf = float_buf(biases)?;
    if sbuf.len() != out * groups || bbuf.len() != out * groups {
        return Err(format!(
            "scales/biases shape mismatch: {}x{} expected, got {}/{}",
            out,
            groups,
            sbuf.len(),
            bbuf.len()
        ));
    }
    let mask: u32 = (1u32 << q.bits) - 1;
    let shift_bits = q.bits as usize;
    let group_size = q.group_size as usize;
    let mut dense = vec![0f32; out * inn];
    for r in 0..out {
        let row_q = &qbuf[r * packed..(r + 1) * packed];
        for (c, cell) in dense[r * inn..(r + 1) * inn].iter_mut().enumerate() {
            let lane = (row_q[c / lanes] >> ((c % lanes) * shift_bits)) & mask;
            let g = c / group_size;
            *cell =
                lane as f32 * sbuf[r * groups + g] + bbuf[r * groups + g];
        }
    }
    let arr = Array::from_slice(&dense, &[out as i32, inn as i32]);
    match scales.dtype() {
        Dtype::Float16 => arr.as_dtype(Dtype::Float16),
        Dtype::Bfloat16 => arr.as_dtype(Dtype::Bfloat16),
        _ => Ok(arr),
    }
    .map_err(|e| format!("cast dequantized: {e}"))
}

// ── norms ─────────────────────────────────────────────────────────────────

/// RMSNorm over the last axis. `weight` is the `[dim]` (or `[head_dim]`)
/// gain; computed in f32 and cast back, matching mlx-lm's `nn.RMSNorm`.
fn rms_norm(x: &Array, weight: &Array, eps: f32) -> MlxResult<Array> {
    let x32 = x.as_dtype(Dtype::Float32)?;
    let mean_sq = ops::square(&x32)?.mean_axes(&[-1], true)?;
    let inv = ops::rsqrt(mean_sq + Array::from_f32(eps))?;
    let out = x32.multiply(&inv)?.multiply(weight)?;
    out.as_dtype(x.dtype())
}

/// Non-traditional RoPE over `[B, L, H, hd]`, pairs `(d, d + hd/2)`,
/// positions `offset + 0..L`. Implemented with explicit contiguous ops:
/// mlx-rs 0.32's `fast::rope` binding rotates along a different axis
/// contract than its docs imply (verified empirically — magnitudes were
/// not even preserved), so the rotation is done manually.
fn rope_manual(x: &Array, hd: i32, theta: f64, offset: i32) -> MlxResult<Array> {
    let l = x.shape()[1];
    let half = hd / 2;
    // cos/sin tables on the CPU: [L, half] each, then broadcast to
    // [1, L, 1, 2*half] with each pair value duplicated.
    let mut cos_t = Vec::with_capacity((l * half) as usize);
    let mut sin_t = Vec::with_capacity((l * half) as usize);
    for p in offset..offset + l {
        for d in 0..half {
            let freq = theta.powf(-(2.0 * d as f64) / hd as f64);
            let ang = freq * p as f64;
            cos_t.push(ang.cos() as f32);
            sin_t.push(ang.sin() as f32);
        }
    }
    let pair_table = |t: Vec<f32>| -> MlxResult<Array> {
        let a = Array::from_slice(&t, &[l, half]);
        let doubled = ops::concatenate(&[&a, &a], 1)?; // [L, hd]
        doubled
            .as_dtype(x.dtype())?
            .reshape(&[1, l, 1, hd])?
            .contiguous()
    };
    let cos = pair_table(cos_t)?;
    let sin = pair_table(sin_t)?;

    let x1 = x.index((.., .., .., 0..half)).contiguous()?;
    let x2 = x.index((.., .., .., half..hd)).contiguous()?;
    let cos1 = cos.index((.., .., .., 0..half)).contiguous()?;
    let sin1 = sin.index((.., .., .., 0..half)).contiguous()?;
    // rotate_half: (-x2, x1)
    let out1 = x1.multiply(&cos1)?.subtract(&x2.multiply(&sin1)?)?;
    let out2 = x2.multiply(&cos1)?.add(&x1.multiply(&sin1)?)?;
    ops::concatenate(&[&out1, &out2], -1)
}

// ── layers ────────────────────────────────────────────────────────────────

struct Attention {
    q: QLinear,
    k: QLinear,
    v: QLinear,
    o: QLinear,
    q_norm: Array,
    k_norm: Array,
}

struct Expert {
    gate: QLinear,
    up: QLinear,
    down: QLinear,
}

impl Expert {
    fn forward(&self, x: &Array) -> MlxResult<Array> {
        let g = nn::silu(&self.gate.forward(x)?)?;
        let u = self.up.forward(x)?;
        self.down.forward(&g.multiply(&u)?)
    }
}

enum Mlp {
    Dense {
        gate: QLinear,
        up: QLinear,
        down: QLinear,
    },
    Moe {
        gate: Linear,
        experts: Vec<Expert>,
    },
}

struct Layer {
    attn_norm: Array,
    post_norm: Array,
    attn: Attention,
    mlp: Mlp,
}

pub struct Qwen3Model {
    embed: Array, // [vocab, dim] dequantized
    layers: Vec<Layer>,
    final_norm: Array,
    lm_head: Linear,
    cfg: Qwen3Config,
    eos_token_ids: Vec<u32>,
}

fn weight_or_err(w: &W, key: &str) -> Result<Array, String> {
    w.get(key)
        .cloned()
        .ok_or_else(|| format!("missing weight `{key}`"))
}

fn quantized_linear(w: &W, key: &str, q: QuantParams) -> Result<QLinear, String> {
    QLinear::new(w, key, q)
}

fn dense_linear(w: &W, key: &str, q: QuantParams) -> Result<Linear, String> {
    if w.get(&format!("{key}.scales")).is_some() {
        Ok(Linear {
            w: dequantize_to_dense(w, key, q)?,
        })
    } else {
        Ok(Linear {
            w: weight_or_err(w, &format!("{key}.weight"))?,
        })
    }
}

impl Qwen3Model {
    pub fn num_layers(&self) -> usize {
        self.layers.len()
    }

    pub fn eos_token_ids(&self) -> &[u32] {
        &self.eos_token_ids
    }

    pub fn load(cfg: Qwen3Config, w: &W, eos_token_ids: Vec<u32>) -> Result<Self, String> {
        let default_q = cfg
            .default_quant()
            .ok_or("the MLX engine currently requires quantized mlx-community models")?;
        let q_for = |key: &str| cfg.quant_for(key).unwrap_or(default_q);

        let embed = dense_linear(w, "model.embed_tokens", q_for("model.embed_tokens"))?
            .w; // [vocab, dim]
        let lm_head = if cfg.tie_word_embeddings {
            Linear { w: embed.clone() }
        } else {
            dense_linear(w, "lm_head", q_for("lm_head"))?
        };

        let mut layers = Vec::with_capacity(cfg.num_hidden_layers as usize);
        for i in 0..cfg.num_hidden_layers {
            let p = format!("model.layers.{i}");
            let qk = |name: &str| {
                let key = format!("{p}.self_attn.{name}");
                quantized_linear(w, &key, q_for(&key))
            };
            let attn = Attention {
                q: qk("q_proj")?,
                k: qk("k_proj")?,
                v: qk("v_proj")?,
                o: qk("o_proj")?,
                q_norm: weight_or_err(w, &format!("{p}.self_attn.q_norm.weight"))?,
                k_norm: weight_or_err(w, &format!("{p}.self_attn.k_norm.weight"))?,
            };
            let mlp = if cfg.is_moe() {
                // mlx-community names the MoE expert block `switch_mlp`.
                let gate = dense_linear(w, &format!("{p}.mlp.gate"), q_for(&format!("{p}.mlp.gate")))?;
                let mut experts = Vec::with_capacity(cfg.num_experts as usize);
                // mlx-community saves experts either per-expert
                // (`…experts.{e}.gate_proj.*`) or — the common quantized
                // layout — stacked into one `[E, out, …]` tensor per
                // projection. Slice the stack per expert.
                let stacked = |name: &str| -> Option<(Array, Array, Array)> {
                    let base = format!("{p}.mlp.switch_mlp.{name}");
                    let (Some(qw), Some(sc), Some(bi)) = (
                        w.get(&format!("{base}.weight")),
                        w.get(&format!("{base}.scales")),
                        w.get(&format!("{base}.biases")),
                    ) else {
                        return None;
                    };
                    Some((qw.clone(), sc.clone(), bi.clone()))
                };
                let (sg, su, sd) = (stacked("gate_proj"), stacked("up_proj"), stacked("down_proj"));
                for e in 0..cfg.num_experts {
                    let ek = |name: &str, stacked: &Option<(Array, Array, Array)>| -> Result<QLinear, String> {
                        if let Some((qw, sc, bi)) = stacked {
                            let e32 = e;
                            let slice = |a: &Array| -> Result<Array, String> {
                                a.index((e32, .., ..))
                                    .contiguous()
                                    .map_err(|err| format!("slice expert {e}: {err}"))
                            };
                            let key = format!("{p}.mlp.switch_mlp.{name}");
                            let q = q_for(&key);
                            return Ok(QLinear {
                                qweight: slice(qw)?,
                                scales: slice(sc)?,
                                biases: slice(bi)?,
                                group_size: q.group_size,
                                bits: q.bits,
                            });
                        }
                        let key = format!("{p}.mlp.switch_mlp.experts.{e}.{name}");
                        quantized_linear(w, &key, q_for(&key))
                    };
                    experts.push(Expert {
                        gate: ek("gate_proj", &sg)?,
                        up: ek("up_proj", &su)?,
                        down: ek("down_proj", &sd)?,
                    });
                }
                Mlp::Moe { gate, experts }
            } else {
                let dk = |name: &str| {
                    let key = format!("{p}.mlp.{name}");
                    quantized_linear(w, &key, q_for(&key))
                };
                Mlp::Dense {
                    gate: dk("gate_proj")?,
                    up: dk("up_proj")?,
                    down: dk("down_proj")?,
                }
            };
            layers.push(Layer {
                attn_norm: weight_or_err(w, &format!("{p}.input_layernorm.weight"))?,
                post_norm: weight_or_err(w, &format!("{p}.post_attention_layernorm.weight"))?,
                attn,
                mlp,
            });
        }

        let final_norm = weight_or_err(w, "model.norm.weight")?;
        Ok(Self {
            embed,
            layers,
            final_norm,
            lm_head,
            cfg,
            eos_token_ids,
        })
    }

    /// Hidden states for `tokens`, updating the per-layer KV `cache`.
    /// Shape `[1, L, dim]`.
    pub fn forward_hidden(
        &self,
        tokens: &[u32],
        cache: &mut [Option<(Array, Array)>],
    ) -> MlxResult<Array> {
        let l = tokens.len() as i32;
        let ids = Array::from_slice(tokens, &[l]);
        let mut h = self.embed
            .take_axis(&ids, 0)?
            .expand_dims_axes(&[0])?
            .contiguous()?;

        // Built-in causal masking for prefill; none for the single-token
        // decode step (attend to everything cached).
        let causal = l > 1;
        for (i, layer) in self.layers.iter().enumerate() {
            h = self.layer_forward(layer, &h, causal, &mut cache[i])?;
        }
        rms_norm(&h, &self.final_norm, self.cfg.rms_norm_eps)
    }

    fn layer_forward(
        &self,
        layer: &Layer,
        h: &Array,
        causal: bool,
        cache: &mut Option<(Array, Array)>,
    ) -> MlxResult<Array> {
        let cfg = &self.cfg;
        let hd = cfg.head_dim;
        let heads = cfg.num_attention_heads;
        let kv_heads = cfg.num_key_value_heads;
        let l = h.shape()[1];
        let normed = rms_norm(h, &layer.attn_norm, cfg.rms_norm_eps)?;
        let a = &layer.attn;
        // QK RMSNorm and RoPE run on the contiguous [B, L, H, hd] layout
        // (mlx-lm's order); reductions over strided transposed views read
        // garbage in mlx-rs 0.32, so transpose only after both.
        let offset = cache.as_ref().map_or(0, |(ck, _)| ck.shape()[2]);
        let rope = |t: &Array, norm: &Array| -> MlxResult<Array> {
            let t = rms_norm(t, norm, cfg.rms_norm_eps)?;
            rope_manual(&t, hd, cfg.rope_theta, offset)
        };
        // mlx-rs 0.32 misreads strided (transposed) views in reductions and
        // fast kernels — materialize contiguous copies at every boundary.
        let q = rope(&a.q.forward(&normed)?.reshape(&[1, l, heads, hd])?, &a.q_norm)?
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?;
        let k = rope(&a.k.forward(&normed)?.reshape(&[1, l, kv_heads, hd])?, &a.k_norm)?
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?;
        let v = a
            .v
            .forward(&normed)?
            .reshape(&[1, l, kv_heads, hd])?
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?;

        let (k, v) = match cache.take() {
            Some((ck, cv)) => (
                ops::concatenate(&[&ck, &k], 2)?,
                ops::concatenate(&[&cv, &v], 2)?,
            ),
            None => (k, v),
        };
        // SDPA handles GQA natively — KV heads must NOT be pre-tiled
        // (mlx-rs fast::scaled_dot_product_attention contract).
        let _ = (heads, kv_heads);
        let scale = 1.0 / (hd as f32).sqrt();
        let mask = causal.then_some(fast::ScaledDotProductAttentionMask::Causal);
        let att = fast::scaled_dot_product_attention(q, &k, &v, scale, mask, None)?;
        let att = att
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?
            .reshape(&[1, l, heads * hd])?;
        let attn_out = a.o.forward(&att)?;

        let h = h.add(&attn_out)?;
        let normed = rms_norm(&h, &layer.post_norm, cfg.rms_norm_eps)?;
        let mlp_out = match &layer.mlp {
            Mlp::Dense { gate, up, down } => {
                let g = nn::silu(&gate.forward(&normed)?)?;
                let u = up.forward(&normed)?;
                down.forward(&g.multiply(&u)?)?
            }
            Mlp::Moe { gate, experts } => self.moe_forward(gate, experts, &normed)?,
        };
        let out = h.add(&mlp_out)?;
        *cache = Some((k, v));
        Ok(out)
    }

    /// Top-k softmax MoE with per-expert gather (mlx-lm routing semantics,
    /// batched by expert instead of masked over all experts so prefill stays
    /// ~k/E FLOPs). `x` is `[1, L, dim]`.
    fn moe_forward(&self, gate: &Linear, experts: &[Expert], x: &Array) -> MlxResult<Array> {
        let n = x.shape()[1];
        let dim = x.shape()[2];
        let k = self.cfg.num_experts_per_tok;
        let _e_total = experts.len();
        let slots = n * k;

        let logits = gate.forward(x)?; // [1, n, E]
        let neg = Array::from_f32(0.0f32) - logits.clone();
        let idx = ops::argpartition_axis(neg, k - 1, -1)?;
        let idx = idx.index((.., .., ..k));
        let top = take_along_axis(&logits, &idx, -1)?;
        let weights = ops::softmax_axis(&top, -1, Some(true))?;

        // One row per (token, slot) pair.
        let flat_x = x.reshape(&[n, dim])?;
        let token_of_slot: Vec<u32> = (0..n)
            .flat_map(|t| std::iter::repeat_n(t as u32, k as usize))
            .collect();
        let token_of_slot = Array::from_slice(&token_of_slot, &[slots]);
        let xe = flat_x.take_axis(&token_of_slot, 0)?; // [slots, D]

        // Group slots by expert id.
        let flat_idx = idx.reshape(&[slots])?;
        let order = ops::argsort_axis(flat_idx.clone(), 0)?;
        let sorted_e: Vec<u32> = flat_idx
            .take_axis(&order, 0)?
            .as_slice::<u32>()
            .to_vec();
        let sorted_x = xe.take_axis(&order, 0)?;

        let mut start = 0usize;
        let mut contrib_parts: Vec<Array> = Vec::new();
        for (e, expert) in experts.iter().enumerate() {
            let mut end = start;
            while end < slots as usize && sorted_e[end] == e as u32 {
                end += 1;
            }
            if end > start {
                let rows = sorted_x.index((start as i32..end as i32, ..));
                contrib_parts.push(expert.forward(&rows)?);
            }
            start = end;
        }
        let y_sorted = ops::concatenate(&contrib_parts.iter().collect::<Vec<_>>(), 0)?;

        // Weight each slot by its router weight, then invert the sort.
        let w_flat = weights.reshape(&[slots])?;
        let w_sorted = w_flat.take_axis(&order, 0)?;
        let w_sorted = w_sorted
            .expand_dims_axes(&[-1])?
            .as_dtype(y_sorted.dtype())?;
        let y_sorted = y_sorted.multiply(&w_sorted)?;
        let unsort = ops::argsort_axis(order.clone(), 0)?;
        let y = y_sorted.take_axis(&unsort, 0)?; // slot order

        // Sum the k slots per token back into one row.
        let y = y.reshape(&[n, k, dim])?;
        let y = ops::sum_axes(&y, &[1], false)?.expand_dims_axes(&[0])?;
        Ok(y)
    }

    /// Debug/audit hook: hidden states after the first `layer_count` layers
    /// (final norm NOT applied). Numerical-parity tests use this to compare
    /// the engine against a CPU reference on real weights.
    #[doc(hidden)]
    pub fn debug_hidden_after_layer(
        &self,
        tokens: &[u32],
        layer_count: usize,
        include_moe: bool,
    ) -> MlxResult<Array> {
        let l = tokens.len() as i32;
        let ids = Array::from_slice(tokens, &[l]);
        let mut h = self
            .embed
            .take_axis(&ids, 0)?
            .expand_dims_axes(&[0])?
            .contiguous()?;
        let causal = l > 1;
        let mut cache: Vec<Option<(Array, Array)>> = vec![None; self.layers.len()];
        for (i, layer) in self.layers.iter().enumerate().take(layer_count) {
            h = if include_moe {
                self.layer_forward(layer, &h, causal, &mut cache[i])?
            } else {
                self.layer_attn_only(layer, &h, causal, &mut cache[i])?
            };
        }
        Ok(h)
    }

    fn layer_attn_only(
        &self,
        layer: &Layer,
        h: &Array,
        causal: bool,
        cache: &mut Option<(Array, Array)>,
    ) -> MlxResult<Array> {
        let cfg = &self.cfg;
        let hd = cfg.head_dim;
        let heads = cfg.num_attention_heads;
        let kv_heads = cfg.num_key_value_heads;
        let l = h.shape()[1];
        let normed = rms_norm(h, &layer.attn_norm, cfg.rms_norm_eps)?;
        let a = &layer.attn;
        let offset = cache.as_ref().map_or(0, |(ck, _)| ck.shape()[2]);
        let rope = |t: &Array, norm: &Array| -> MlxResult<Array> {
            let t = rms_norm(t, norm, cfg.rms_norm_eps)?;
            rope_manual(&t, hd, cfg.rope_theta, offset)
        };
        let q = rope(&a.q.forward(&normed)?.reshape(&[1, l, heads, hd])?, &a.q_norm)?
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?;
        let k = rope(&a.k.forward(&normed)?.reshape(&[1, l, kv_heads, hd])?, &a.k_norm)?
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?;
        let v = a
            .v
            .forward(&normed)?
            .reshape(&[1, l, kv_heads, hd])?
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?;
        let (k, v) = match cache.take() {
            Some((ck, cv)) => (
                ops::concatenate(&[&ck, &k], 2)?,
                ops::concatenate(&[&cv, &v], 2)?,
            ),
            None => (k, v),
        };
        let scale = 1.0 / (hd as f32).sqrt();
        let mask = causal.then_some(fast::ScaledDotProductAttentionMask::Causal);
        let att = fast::scaled_dot_product_attention(q, &k, &v, scale, mask, None)?;
        let att = att
            .transpose_axes(&[0, 2, 1, 3])?
            .contiguous()?
            .reshape(&[1, l, heads * hd])?;
        let attn_out = a.o.forward(&att)?;
        let out = h.add(&attn_out)?;
        *cache = Some((k, v));
        Ok(out)
    }

    /// Debug/audit hook: the MoE output of `layer` for input `x`
    /// `[1, L, dim]`, plus the routed expert ids per token.
    #[doc(hidden)]
    pub fn debug_moe(
        &self,
        layer: usize,
        x: &Array,
    ) -> MlxResult<(Array, Vec<u32>, Vec<f32>)> {
        let Mlp::Moe { gate, experts } = &self.layers[layer].mlp else {
            unreachable!("debug_moe on a dense layer");
        };
        let k = self.cfg.num_experts_per_tok;
        let logits = gate.forward(x)?;
        let neg = Array::from_f32(0.0f32) - logits.clone();
        let idx = ops::argpartition_axis(neg, k - 1, -1)?;
        let idx = idx.index((.., .., ..k));
        let n = x.shape()[1];
        let flat = idx.reshape(&[n * k])?.contiguous()?;
        let ids: Vec<u32> = flat.as_slice::<u32>().to_vec();
        let top = take_along_axis(&logits, &idx, -1)?;
        let weights = ops::softmax_axis(&top, -1, Some(true))?;
        let wv: Vec<f32> = weights
            .as_dtype(Dtype::Float32)?
            .reshape(&[-1])?
            .contiguous()?
            .as_slice::<f32>()
            .to_vec();
        let out = self.moe_forward(gate, experts, x)?;
        Ok((out, ids, wv))
    }

    /// Greedy next-token logits for the last position.
    pub fn next_token_logits(
        &self,
        tokens: &[u32],
        cache: &mut [Option<(Array, Array)>],
    ) -> MlxResult<Array> {
        let l = tokens.len() as i32;
        let h = self.forward_hidden(tokens, cache)?;
        // Range (not integer) indexing keeps the position axis: [1, 1, dim].
        let last = h.index((.., l - 1..l, ..));
        self.lm_head.forward(&last)
    }

    /// Argmax helper shared with the adapter.
    pub fn argmax_token(logits: &Array) -> MlxResult<u32> {
        let best = argmax_axis(logits, -1, None)?;
        Ok(best.item_exact::<u32>())
    }
}
