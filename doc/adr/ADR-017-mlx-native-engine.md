# ADR-017 — A native in-process MLX engine, feature-gated, beside GGUF

Status: accepted. Date: 2026-09-19. Supersedes the "native MLX deferred"
clause of ADR-016 (the loopback-server path stays available; it is no longer
the only MLX route).

## Context

ADR-016 routed MLX weights through user-run loopback OpenAI-compatible
servers (LM Studio, `mlx_lm.server`) because `mlx-lm` is Python (barred by
the stack contract) and a hand-rolled MLX runner looked like duplicating a
model runtime. The user's machine holds `mlx-community` 4-bit checkpoints
and the dependency on an external app was unwanted.

Revised assessment:

- `mlx-rs` (0.32, actively maintained, follows MLX versioning) exposes the
  needed surface: `quantized_matmul`, `dequantize`, safetensors I/O, fast
  SDPA with native GQA, Metal by default.
- The runtime to write is not a model framework — it is one architecture
  family (Qwen3 dense + MoE) over raw arrays: ~700 lines of graph code.

## Decision

Ship `oxibrain-llm-mlx` (crate, CLI feature `mlx`, off by default) with a
`Qwen3`/`Qwen3-MoE` runner over raw MLX arrays:

- **Engine choice is per manifest entry**: `format = "gguf"` (default) loads
  through llama.cpp with GBNF; `format = "mlx"` loads MLX safetensors
  in-process. GGUF stays the C2 zero-setup default; MLX is an opt-in build
  feature and an opt-in manifest entry.
- **Mechanism is `JsonMode`**: MLX has no grammar engine, so capabilities
  advertise nothing and the extraction pipeline takes schema-and-repair with
  the validator as the gate (§9.4). Same contract as every non-GBNF adapter.
- **All MLX work happens on one dedicated worker thread** — MLX streams are
  thread-local and arrays may only be evaluated on their creating thread.
- **Cache identity (§9.5)**: MLX entries carry a fingerprint (blake3 over
  `config.json` + sorted shard names and byte sizes). A full 17 GB re-read
  per store open would cost more than extraction itself; the fingerprint
  pins the artifact set. In-place weight edits keeping identical sizes would
  evade it — accepted for v1.

### mlx-rs 0.32 binding landmines (all verified against ground truth,
worked around in the engine)

1. `ops::dequantize` returns wrong values for non-square packed shapes
   (a square-weight test hides the orientation bug). Replaced with a CPU
   unpacker implementing MLX affine quantization directly (unsigned
   LSB-first lanes, `value = lane·scale + group_bias`), byte-exact against
   `quantized_matmul`.
2. Reductions and several ops read garbage from strided (transposed) views.
   Every transpose boundary materializes a `.contiguous()` copy.
3. Axis-less `ops::softmax` does **not** normalize over the last axis (MoE
   router weights summed to ~0.86, a uniform output deficit). The engine
   uses explicit `softmax_axis(.., -1, ..)`.
4. `fast::rope`'s binding does not honour the documented layout contract
   (rotations were not even magnitude-preserving in probes). RoPE is
   implemented manually: CPU cos/sin tables, explicit rotate-half over
   contiguous halves.

A `graph_parity` test runs the full engine against an independent f64 CPU
reference forward (attention + MoE + norms) on a synthetic quantized model,
and debug hooks compare engine layers against CPU on the real 30B weights.

### Known cost of this release

**Statically linking llama.cpp (`oxibrain-llm-local`, `oxibrain-embed-local`)
and mlx-c in one binary corrupts llama.cpp's GGUF parsing** — the bge-m3
embedder segfaults in `gguf_get_key` in `--features mlx` builds. MLX builds
therefore skip the GGUF embedder: dense retrieval is unavailable, lexical
and graph channels keep working (a loud warning is logged). GGUF-only builds
are unaffected. Removing the conflict (separate process, or symbol
isolation) is future work; it does not affect extraction, which never
touches the embedder.

## Consequences

- The user's local `mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit` runs
  in-process (measured: "Bibimbap is a famous food in Seoul.", extraction
  drain 1 accepted / 0 rejected on the first episode).
- Extraction quality comparisons stay per-`ExtractorId` (§17.2): MLX runs
  are labelled `JsonMode` with their own model id and fingerprint.
- The loopback provider (ADR-016) remains for servers users already run.
- Non-Apple platforms and the standalone default build pull no MLX
  toolchain; CI matrix needs a macOS + `--features mlx` leg for this crate.
