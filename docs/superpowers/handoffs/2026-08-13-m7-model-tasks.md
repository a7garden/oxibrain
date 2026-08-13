# Handoff — M7 Model Tasks Complete (7.1–7.7, 7.13 + spike)

> **Status:** M7 fully complete. All 10 remaining M7 tasks shipped: spike (§2),
> 7.1 (llm-local), 7.2 (constrained decoding), 7.3 (embed-local), 7.4
> (TokenizerPort), 7.5 (model artifacts), 7.6 (dense path), 7.7 (quantize),
> 7.13 (parity corpus). ADR-003 records the spike outcome.
> **Branch:** `main` · **Predecessor:** `2026-08-13-m7-language-primitives.md`
> **Tests:** 53 test binaries pass, 0 fail. Clippy `-D warnings` clean. Fmt clean.
> **Spec:** `doc/spec/M7-model-and-language.md` · **Roadmap:** `doc/ROADMAP.md` §2

---

## What shipped this session (7 commits)

| Commit | Task | Content |
|---|---|---|
| `d6e0712` | 7.2, 7.7, 7.4 | `grammar_from_registry` GBNF + LlmPort expansion; `quantize` module; `TokenizerPort` + `estimate_tokens_rough` |
| `c1a0396` | Spike §2 | GBNF spike validates llama-cpp-2 grammar-constrained decoding; ADR-003 |
| `a2929dc` | 7.1 | `oxibrain-llm-local` crate (GGUF behind LlmPort + TokenizerPort) |
| `ddd65d6` | 7.3 | `oxibrain-embed-local` crate (multilingual encoder behind EmbeddingPort) |
| `71226f3` | 7.5 | model artifacts: manifest, pull/verify/use CLI, `model_digest` → ExtractorId |
| `9a1d438` | 7.6 | dense path: `upsert_vector` wired into reproject (F17), `dense_search` KNN (F16) |
| `00fd6a2` | 7.13 | parity corpus skeleton: 7 languages × 2 annotated episodes + validation test |
| `844c1a5` | docs | §21 strikes F15/F16/F17/F25/F26/F27; §16.3 measured latency; ARCHITECTURE.md v2.1 |

---

## Key architectural changes

### 1. Spike outcome → llama-cpp-2 is the engine (ADR-003)

`doc/adr/ADR-003-local-inference-engine.md` records: grammar-constrained
decoding works end-to-end; custom `grammar_from_registry` output parses as
valid GBNF; all parse failures in the spike were **truncation** (max_tokens),
not grammar issues. Extraction quality with small models is the M7→gate risk
(decision-table row 2). Key GBNF findings:

- **Rule names use hyphens, not underscores** (`object-union`, not `object_union`)
- **Each rule on a single line** (parser treats newlines as rule separators)
- `LlamaSampler::grammar(model, grammar, "root")` + `greedy()` chain
- `sample()` accepts internally — never double-`accept()` (corrupts grammar state)
- Qwen2.5 has `add_bos_token = false` → `AddBos::Never` for generation
- BERT embedding models need `AddBos::Always` ([CLS]/[SEP])

### 2. Dependency direction (from prior session) now pays off

`oxibrain-core → oxibrain-index` (not reverse). New index algorithms are
available to core via `oxibrain_index::*`. `PerType<T>` in
`core/resolution.rs`. `FtsIndex` enum in `store/query.rs` (not public).

### 3. `oxibrain-llm-local` (7.1)

- `LocalLlm::open(path, LocalLlmOptions)` → `LlmPort` + `TokenizerPort`
- Metal on aarch64-apple-darwin (n_gpu_layers), CPU everywhere
- Generation on `spawn_blocking` (never the writer actor, §9.2)
- `BrainError::Model` added for missing/corrupt weights (non-retryable)
- Integration tests (ignored, need `~/.oxi/models/qwen2.5-1.5b-instruct-q4_k_m.gguf`):
  `model_loads_and_tokenizer_counts`, `model_generates_with_and_without_grammar`

### 4. `oxibrain-embed-local` (7.3)

- `LocalEmbedder::open(path, opts)` → `EmbeddingPort` (dim + embed, L2-normalized)
- **BGE-M3 GGUF has `LLAMA_POOLING_TYPE_NONE`**: multi-sequence batching fails
  (`NTokensZero`); process one text per decode. `embeddings_seq_ith(0)` works
  per single sequence.
- Cross-lingual validation: EN-KO 0.893, EN-JA 0.881 cosine on same-meaning
  sentences (P11).
- Integration test (ignored): `embedder_loads_and_embeds`.

### 5. Model artifacts (7.5)

- `crates/oxibrain/src/models.rs`: `ModelEntry {role, name, file, url, digest, size_mb, license}`;
  manifest at `~/.oxi/models/manifest.toml`; blake3 `digest_file`, `verify_entry`
  (corruption detection); `pull_entry` resumable (HTTP Range on `.part`) with
  progress + digest verification before finalize.
- CLI: `oxibrain model {list,pull,verify,use}`.
- `ExtractorConfig` gains `model_digest: Option<String>` hashed into
  `ExtractorId` (§9.5). Test `extractor_id_changes_with_digest` verifies weight
  changes invalidate the id. All 8 constructors updated.
- Default manifest: Qwen2.5-1.5B-Instruct (extract) + BGE-M3 (embed), real blake3 digests.

### 6. Dense path (7.6, F16/F17)

- `QueryMode::Dense` added. `dense_search(conn, embedder, query, limit)` embeds
  the query and KNNs via sqlite-vec.
- `hybrid_query` takes `Option<&dyn EmbeddingPort>`: explicit Dense mode errors
  without an embedder (no silent fallback); Hybrid includes the dense channel
  only when an embedder is present.
- `embed_entities` wired into reproject (F17): entity texts read via readers,
  embeddings computed outside any writer lock, upserts submitted to the writer.
  Split into `entity_embedding_texts` (read) / `upsert_entity_embeddings` (write)
  phases so no transaction spans an embedding computation (P9).
- Schema v7: `entity_vectors` recreated at 1024-dim (BGE-M3; v5 was 384-dim
  all-MiniLM, never populated). `EMBEDDING_DIM = 1024`.
- MCP `parse_mode` gains `"dense"`. Brain gains `with_embedder()`.
- Tests: `m7_dense.rs` — reproject embeds + dense KNN finds; Dense without
  embedder returns explicit error.

### 7. Parity corpus (7.13)

`eval/parity/{en,es,ko,ja,zh,ar,th}/` — 14 episodes (2 each), annotated with
entities (verbatim surfaces + **byte** spans), expected statements (registry
predicates), 2-3 questions + reference answers. `manifest.toml` maps each
language to the D31 properties it exercises. Byte spans computed from UTF-8
offsets, round-trip verified.

`tests/parity_corpus.rs` validates: 7 languages present, surfaces verbatim at
spans (fabricated-entity gate), predicates in registry, manifest matches disk.

### 8. Findings struck (§21)

F15 (embed-local), F16 (dense path), F17 (upsert caller), F25/F26 (CJK
tokenization → model tokenizer), F27 (chars/4 → TokenizerPort). §16.3 local
extraction latency row: **~13 s** (Qwen2.5-1.5B Q4_K_M, M4 Metal, 512 tok).
ARCHITECTURE.md v2.0 → v2.1.

---

## Verification snapshot

```
$ cargo test --workspace
53 test binaries, 0 failed

$ cargo clippy --workspace --all-targets -- -D warnings
Finished (clean)

$ cargo fmt --all -- --check
(clean)

$ cargo build -p oxibrain --no-default-features --features http-llm
Finished (standalone build)

$ cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'
CLEAN: no oxi-ecosystem deps

# Model-backed integration tests (ignored by default; need ~/.oxi/models/):
$ cargo test -p oxibrain-llm-local --test local_model -- --ignored
2 passed (tokenizer + grammar-constrained generation)
$ cargo test -p oxibrain-embed-local --test local_embedder -- --ignored
1 passed (BGE-M3 cross-lingual: EN-KO 0.893, EN-JA 0.881)
```

---

## Remaining M7 acceptance items

- [ ] **§5.1 ranking tolerance calibration** (the one open M7 checkbox). Needs:
  1. Create `eval/probes/` with a fixed probe set (recall@10 queries)
  2. Measure on CPU and Metal, ten runs each
  3. Set tolerance to `max(2pp, 2 × observed_max_delta)`; write number + date
     into ARCHITECTURE.md §5.1, replacing the "calibrated, not guessed" text
  4. Update `ranking_equivalence` test to use the tolerance
  This is the documented follow-up from the 7.8 split.

---

## Next session: M8 — The decide layer (ROADMAP §3)

M8 is ≈20–25 days: `core::rank` pure (8.1), `Retrieval` type (8.2),
`store::retrieve` (8.3), conservation property tests (8.4), belief-filtered
adjacency + `traverse().as_of(t)` (8.5, F11), `known_at` (8.6, F8),
`core::pack` pure (8.7), Profile layer (8.8, D21), `render_belief` rewrite
(8.9, F6), expansion policy (8.10, F7), `chunks` table + migration v7 (8.11 —
note: **our v7 is already taken** by the 1024-dim vectors; chunks becomes v8),
MCP additive params (8.12, F29/F30), `why --dropped` real data (8.13, F2).
Exit: facade under 1,500 LOC (from 3,067).

Then the gate (ROADMAP §4): three-arm comparison, ~5 days.

## Critical context

- **Schema version is now 7** (v7 = 1024-dim entity_vectors). M8's chunks
  migration becomes v8. Migration tests assert `expected: 7`.
- **`hybrid_query(conn, q, embedder: Option<&dyn EmbeddingPort>)`** — 3 callers:
  facade `query()` (passes `self.embedder.as_deref()`), `store/context.rs`
  (passes `None`), bench (via facade).
- **LocalLlm/ LocalEmbedder share one `OnceLock<LlamaBackend>`** per crate —
  two backends exist process-wide (one per crate). Fine; init is idempotent.
- **Grammar rule names use hyphens** — `grammar_from_registry` output is valid
  GBNF for llama.cpp. Do not revert to underscores.
- **`model_digest: Option<String>` on ExtractorConfig** — all constructors set
  `None`; wire the manifest digest when building a local extractor config.
- **`EMBEDDING_DIM = 1024`** in `store/vectors.rs` — the vec0 table matches
  BGE-M3. A different embedder dimension needs a new table.
- **Parity corpus byte spans are byte offsets** (UTF-8), matching the
  extraction schema's `span: [u32, u32]`.
- **Test count:** 53 test binaries (was 51 at session start; +m7_dense, +parity_corpus).
- **Models downloaded** to `~/.oxi/models/` (gitignored): qwen2.5-1.5b (extract),
  bge-m3 Q4_K_M (embed), qwen2.5-0.5b (spike only, can be deleted).
- **Branch:** `main`. All commits direct.
