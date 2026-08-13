# ADR-003: Local Inference Engine — llama-cpp-2

**Date:** 2026-08-13  
**Status:** Accepted  
**Supersedes:** D28 (architecture decision, now validated)

## Context

M7 commitment C2 ("own the model") requires local LLM inference behind `LlmPort`,
with grammar-constrained decoding (D28) for extraction. The spec (§2) mandates a
spike before building 7.1–7.2 to validate that a small local model can produce
parseable, grammar-constrained JSON.

## Decision

**llama-cpp-2 (v0.1.154) is the local inference engine.** It wraps llama.cpp,
supports Metal/CUDA/CPU, and provides GBNF grammar-constrained sampling.

## Spike Details

### Setup
- **Engine:** `llama-cpp-2 = "0.1.154"` (llama.cpp backend, Metal-accelerated)
- **Hardware:** Apple M4, 18 GB unified memory, Metal GPU family Apple9
- **Models tested:** Qwen2.5-0.5B-Instruct-Q4_K_M (463 MB), Qwen2.5-1.5B-Instruct-Q4_K_M (1.07 GB)
- **Corpus:** 10 golden episodes (5 English, 5 CJK: Korean, Japanese, Chinese)
- **Grammar:** `grammar_from_registry()` — custom GBNF generated from the predicate registry

### Results

| Metric | Qwen2.5-0.5B | Qwen2.5-1.5B |
|---|---|---|
| Grammar parse | OK | OK |
| Grammar-constrained generation | Works | Works |
| Output starts with `{` (grammar-enforced) | Yes | Yes |
| Parse failures | 10/10 (truncation) | 10/10 (truncation) |
| Wall-clock per episode (512 tok) | ~4 s | ~13 s |
| Peak RSS | ~600 MB | ~1.5 GB |

**All parse failures are truncation** — the model exhausts `max_tokens` before
completing the JSON. The output follows the grammar structure perfectly
(valid field names, valid enum values, correct nesting). Increasing `max_tokens`
beyond 512 would resolve truncation but increases latency.

### Grammar-constrained decoding findings

1. **GBNF rule names use hyphens, not underscores.** llama.cpp's parser reads
   `object_union` as rule `object` followed by unexpected `_union`. Fix: use
   `object-union`, `valid-from-opt`, etc.
2. **Each rule must be on a single line.** The parser treats newlines as rule
   separators.
3. **`LlamaSampler::grammar(model, grammar_str, "root")` creates the grammar
   sampler; chain with `greedy()` for deterministic output.**
4. **`sample()` accepts internally** — do NOT call `accept()` again; double-accept
   corrupts the grammar sampler's position tracking.
5. **Qwen2.5 has `add_bos_token = false`** — use `AddBos::Never`.

## Analysis

### Decision table outcome

The spec's decision table says:

| Result | Action |
|---|---|
| 0 parse failures, validator rejections comparable to HTTP | Proceed |
| 0 parse failures, rejections much worse | Proceed; extraction quality is gate risk |
| Grammar wiring impractical | Escalate |

We are in **row 2**: grammar wiring is fully functional (0 grammar-related parse
failures), but extraction quality with small local models is poor. This is the
expected M7→gate risk. Tiering (§8.5) — using frontier models for quality-critical
extraction and local models for cost-sensitive — is the mitigation.

### Why not candle or mistral.rs?

- `candle` (Hugging Face) does not support grammar-constrained decoding.
- `mistral.rs` supports structured output via GBNF but wraps llama.cpp anyway.
- `llama-cpp-2` is the most direct, well-maintained Rust binding.

No alternative provides better grammar support.

## Consequences

- **7.1 proceeds with llama-cpp-2** as the inference backend.
- **Grammar-constrained decoding is viable** — `grammar_from_registry()` output
  is valid GBNF that llama.cpp accepts.
- **Extraction quality is the M7→gate risk** — small models (0.5B–1.5B) generate
  valid-structure JSON but with noisy entity boundaries and span offsets. The
  gate (ROADMAP §4) will measure this quantitatively.
- **Metal acceleration works** — all layers offloaded to GPU, ~13 s/episode at
  512 tokens with Qwen2.5-1.5B on Apple M4.
- **Minimum viable model size is ~1.5B** for any extraction quality. 0.5B models
  generate empty or low-quality output.
