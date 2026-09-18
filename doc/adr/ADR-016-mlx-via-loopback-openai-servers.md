# ADR-016 — MLX models are served through loopback OpenAI-compatible servers, not a native engine

Status: accepted. Date: 2026-09-19.

## Context

Apple MLX is the fastest way to run quantized LLMs on Apple Silicon
(`mlx-community` 4-bit quants of Qwen3-30B-A3B run well past what the bundled
GGUF extract model reaches). Users hold MLX weights already. oxibrain's
extract path needs **structured output that survives validation** (§9.4,
§9.5): the default local path enforces it with GBNF grammar-constrained
decoding through llama.cpp (D28), and the validator gates correctness after
any mechanism.

Three ways to reach MLX weights:

1. **Shell out to `mlx-lm` (Python).** Rejected outright: the product
   guarantees no Python runtime and no external toolchain (§1.4, "no
   Python" in the stack contract).
2. **A native MLX engine in Rust** (`mlx-rs` bindings, hand-rolled model
   runners: KV cache, quantized linear, per-architecture graphs). This
   duplicates a model runtime inside oxibrain, ships the maintenance
   surface llama.cpp already owns, and — decisive — **forfeits GBNF**:
   MLX has no grammar engine, so every MLX extraction would ride
   schema-and-repair regardless of how much native code we write. The
   architecture already anticipated pure-Rust engines as *adapter swaps*
   re-examined at M-gates (§8), gated on matching the constrained-decoding
   story, not just on producing tokens.
3. **A loopback OpenAI-compatible server the user runs** (LM Studio's MLX
   engine on `127.0.0.1:1234`, `mlx_lm.server`, llama.cpp `server`). The
   server owns MLX weights and inference; oxibrain talks the existing
   `oxibrain-llm-http` tier with `response_format: json_schema`
   (mechanism `JsonSchema` → schema-and-repair; validator unchanged).

## Decision

Option 3, as a first-class provider — not a hack:

- `OpenAiLlm::with_base_url(base, api_key: Option<_>, model)` generalizes
  the existing adapter: loopback servers get no auth header when no key is
  configured.
- `OXIBRAIN_LLM_PROVIDER=loopback` (aliases `lmstudio`, `mlx`) resolves
  `OXIBRAIN_LLM_MODEL` (required, never guessed), `OXIBRAIN_LLM_BASE_URL`
  (default `http://127.0.0.1:1234/v1`), optional `OXIBRAIN_LLM_API_KEY`.
- Mechanism is `JsonSchema`; `LlmCapabilities.grammar` stays `false`, so
  the pipeline takes its existing schema-and-repair branch. Trust and
  validation invariants are untouched; the tiering story (§8.5) gains an
  explicit "local server, structured output" rung without displacing the
  C2 local-GGUF default.

## Consequences

- MLX acceleration arrives with ~zero new native code and no new build
  dependencies; the default build still runs standalone with nothing
  installed (C2) because loopback is an explicit, named override.
- Extraction over MLX relies on the server's structured-output support
  plus the validator, not grammar constraints. Measured quality splits by
  `ExtractorId` (§17.2) as for any other provider change.
- A native in-process MLX engine remains open, gated on the same M-gate
  question as candle/mistral.rs: it must bring constrained decoding or a
   demonstrated quality win worth the duplicated runtime.
- Doc drift noted and fixed alongside: the §18 crate table said
  "anthropic/openai/ollama"; the ollama adapter never existed — loopback
  supersedes it for that use case.
