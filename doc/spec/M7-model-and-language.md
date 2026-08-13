# M7 — Own the Model · Implementation Spec

> **Version:** v1.0 · **Date:** 2026-08-13 · **Status:** Ready for implementation
> **Authority:** `doc/ARCHITECTURE.md` v2.0. Where this spec and ARCHITECTURE.md disagree,
> ARCHITECTURE.md wins and this spec is wrong.
> **Sequencing:** `doc/ROADMAP.md` §2. **Conventions:** `AGENTS.md`.
> **Convention:** `§n` refers to `doc/ARCHITECTURE.md`; `F n` to its §21.

---

## 1. What M7 is

M7 delivers commitments **C2 (own the model)** and **C3 (language independence)**, plus the
**P1 truth/ranking split** that C2 would otherwise break.

It adds almost no user-visible features. It makes the product work without an API key, work
outside English, and — for the first time — be measurable against a control.

### In scope

- Local inference and local embeddings behind the existing ports (§8.2)
- Grammar-constrained decoding generated from the predicate registry (§9.4, D28)
- `TokenizerPort` and exact token counting (§7.5)
- Model artifact management: fetch, pin, verify (§8.4)
- Character n-gram primitives and the dual FTS index (§7.3, §7.4)
- Name similarity without the prefix bonus (§7.7)
- The `snapshot_truth` / `snapshot_ranking` split with a **measured** tolerance (§5.1)
- The parity corpus skeleton (§7.8)

### Explicitly not in scope

- `core::rank` / `core::pack` — M8. M7 must not refactor retrieval.
- Chunking and the `chunks` table — M8 (8.11). M7 indexes episodes and statements as today.
- Reranking, MMR — M10.
- Blocking (MinHash/LSH as a *resolution* candidate generator) — M9. M7 ships the `ngram`
  module those will use, with property tests, but does not wire it into resolution beyond
  replacing the similarity function (7.12).

Keeping M8's refactor out of M7 matters: M7 already touches the schema, the ports, and the
index layer. Adding a retrieval rewrite makes the milestone unreviewable.

---

## 2. Prerequisite: the spike

**Do this before anything else. It is one day and it can invalidate 7.1–7.2.**

`spike/gbnf/` (throwaway, not a workspace member):

1. Take the existing registry-generated JSON Schema from
   `oxibrain_core::extraction::schema_from_registry`.
2. Convert it to GBNF.
3. Run a quantized multilingual instruct model via `llama-cpp-2` over **ten golden-corpus
   episodes**, five English and five non-Latin-script.
4. Record: parse-failure rate (must be 0), validator-rejection rate, wall-clock per episode,
   peak RSS.

**Decision:**

| Result | Action |
|---|---|
| 0 parse failures, validator rejections comparable to the HTTP extractor | Proceed. `llama-cpp-2` is the engine |
| 0 parse failures, validator rejections much worse | Proceed with 7.1–7.2, and treat extraction quality as the M7→gate risk. Tiering (§8.5) is the mitigation |
| Grammar wiring is impractical | Re-evaluate `candle` / `mistral.rs`. If none supports constrained decoding, **escalate**: D28 is wrong and C2's cost changes |

Record the outcome in `doc/adr/ADR-003-local-inference-engine.md` either way. This is exactly
the kind of choice that should be an ADR.

---

## 3. Tasks

Ordered so the tree stays green. Each task is one commit.

### 7.9 — `oxibrain-index::ngram` *(do first: pure, no dependencies)*

```rust
// crates/oxibrain-index/src/ngram.rs

/// Character n-gram shingles over a normalized string.
/// Language-independent by construction (P11): no word boundaries, no stemming.
/// Pads with a sentinel so short strings still produce shingles.
pub fn shingles(s: &str, n: usize) -> BTreeSet<String>;

/// Jaccard similarity over shingle sets. Order-insensitive, prefix-neutral (§7.7).
pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64;

/// Shannon entropy over the shingle distribution. Gates the fuzzy path (§10.1):
/// short or low-entropy strings produce unreliable shingle sets in ANY script.
pub fn shingle_entropy(sh: &BTreeSet<String>) -> f64;

/// Deterministic MinHash signature. Seeded, fixed permutation count.
pub fn minhash(sh: &BTreeSet<String>, perms: usize) -> Vec<u64>;

/// LSH bands over a MinHash signature, for candidate generation (M9).
pub fn lsh_bands(sig: &[u64], band_size: usize) -> Vec<u64>;
```

**Property tests (required):**

- `jaccard(a, a) == 1.0`; `jaccard(a, ∅) == 0.0`; symmetry.
- **Script invariance:** for a fixed edit operation (swap two characters), the Jaccard delta is
  within a tolerance across Latin, Hangul, Han and Arabic inputs. This is the test that encodes
  P11 for this module.
- `minhash` is deterministic across runs and platforms.
- `shingles` never returns empty for a non-empty input.

**Anti-requirement:** this module must contain no word list, no stemmer, and no script check
(§18 rule 6). It is the only crate permitted to contain such a thing, and it should not need to.

### 7.12 — Name similarity

`crates/oxibrain-core/src/resolution.rs`:

- Delete `w_alias` (F14) and the `strsim` dependency.
- `ResolutionConfig { tau_high, tau_low, w_exact, w_ngram, w_graph, w_embedding: PerType<f64> }`.
- `score()` uses `ngram::jaccard` over 3-gram shingles of the normalized surfaces.
- `w_graph` and `w_embedding` stay wired but may remain zero until M9; **do not delete them,
  and do not hardcode the caller's argument to `0.0`** — that is exactly how F13 happened.

Re-tune `tau_high` / `tau_low` against the golden corpus: Jaccard and Jaro-Winkler are on
different scales, so the v1.0 thresholds (0.85 / 0.55) are meaningless for the new metric. Record
the new values and the measurement in the commit message.

### 7.11 — Fallback vectors on n-grams

`crates/oxibrain-index/src/vector.rs`:

- Delete `STOP_WORDS` (F23) and the `s.len() > 1` byte filter (F24).
- `tokenize()` → `features()`, returning character n-grams from `ngram::shingles`.
- Keep the hashing trick and fixed dimensionality — determinism is unchanged.
- Rename the retrieval channel from `semantic` to `lexical-vector` wherever it is user-visible.
  Calling a hashed bag-of-shingles "semantic" is how F16 survived review.

### 7.10 — Migration v6: dual FTS

`crates/oxibrain-store/src/migrations/v6.sql`:

```sql
DROP TABLE IF EXISTS episodes_fts;

CREATE VIRTUAL TABLE fts_word USING fts5(
    body,
    space_id    UNINDEXED,
    target_kind UNINDEXED,
    target_id   UNINDEXED,
    tokenize = 'unicode61'          -- porter removed (F22)
);

CREATE VIRTUAL TABLE fts_ngram USING fts5(
    body,
    space_id    UNINDEXED,
    target_kind UNINDEXED,
    target_id   UNINDEXED,
    tokenize = 'trigram'
);
```

`index_ops.rs::rebuild_fts` populates **both**, always. No script detection, no routing (§7.4).

`query.rs::fts_search` gains an index parameter and returns a `SearchHit` list per index; both
lists enter RRF as separate channels. This is a *pre-echo* of M8's `Channel` enum — keep it
minimal, do not build the full `Retrieval` type here.

**Migration test:** an up-test from a v5 fixture, per `AGENTS.md`. Assert both tables exist and
are populated after `rebuild_indexes`, and that a v5 database opens and reprojects cleanly.

### 7.8 — The truth/ranking split *(land before any vector writer)*

`crates/oxibrain-store/src/index_ops.rs`:

```rust
/// Byte-identical across rebuilds. The strict contract (P1).
pub fn snapshot_truth(conn: &Connection, space: &str) -> Result<String, BrainError>;

/// Membership + probe recall. The equivalent contract (P1).
pub fn snapshot_ranking(conn: &Connection, space: &str) -> Result<RankingSnapshot, BrainError>;
```

`snapshot_truth` covers `entities`, `entity_keys`, `entity_merges`, `statements`, `assertions`,
`mentions`, `beliefs`, `predicates`. **It must not include vectors** — that is F18.

`snapshot_ranking` returns target-id sets per index plus a recall@10 measurement over a fixed
probe set stored under `eval/probes/`.

`crates/oxibrain/tests/reproject_determinism.rs` keeps its name and asserts over
`snapshot_truth`. A new `ranking_equivalence.rs` asserts over `snapshot_ranking`.

**Calibrating the tolerance.** After 7.3 and 7.7 land, measure recall@10 on the probe set on CPU
and on Metal, ten runs each. Set the tolerance to `max(2pp, 2 × observed_max_delta)` and
**write the measured number and its date into `doc/ARCHITECTURE.md` §5.1**, replacing the
placeholder. A number carried forward unmeasured is the mistake §17.3 warns about.

### 7.1 — `oxibrain-llm-local`

New crate. Depends on `oxibrain-ports` and the inference backend only.

```rust
pub struct LocalLlm { /* model handle, sampler config, tokenizer */ }

impl LocalLlm {
    pub fn open(path: &Path, opts: LocalLlmOptions) -> Result<Self, BrainError>;
}

impl LlmPort      for LocalLlm { /* generate, generate_constrained */ }
impl TokenizerPort for LocalLlm { /* count, truncate_to */ }
```

Requirements:

- Metal on aarch64-apple-darwin, CUDA when present, CPU everywhere. **CPU must work** — it is
  the portability floor.
- `BrainError::Model { .. }` for missing or corrupt weights; never a panic.
- Generation runs on a blocking thread, never on the writer actor and never inside a
  transaction (§9.2).
- Concurrency capped by config (§9.7): local inference competes with the user's machine, not
  with a rate limit.

### 7.2 — Constrained decoding

`crates/oxibrain-core/src/extraction.rs`:

```rust
/// Generate a GBNF grammar from the predicate registry.
/// Sibling of `schema_from_registry` — one registry, two consumers (P4).
pub fn grammar_from_registry(predicates: &[PredicateDef]) -> String;
```

`oxibrain-ports`:

```rust
pub trait LlmPort: Send + Sync {
    async fn generate(&self, req: &GenerateRequest) -> Result<String, BrainError>;

    /// Constrained generation. Adapters that cannot honour a grammar return
    /// `Err(BrainError::Provider { retryable: false, .. })`; the caller falls back
    /// to schema-and-repair and records the mechanism in ExtractorId (§9.5).
    async fn generate_constrained(&self, req: &GenerateRequest, grammar: &str)
        -> Result<String, BrainError> { let _ = grammar; Err(unsupported()) }

    fn capabilities(&self) -> LlmCapabilities;   // { grammar: bool, structured_output: bool, .. }
}
```

**Test:** the property that matters is that `grammar_from_registry` and `schema_from_registry`
accept the same language. Generate 100 random valid claim sets from the registry, serialize
them, and assert both the JSON Schema validator and a GBNF parse accept every one — and that a
mutated invalid set is rejected by both.

### 7.3 — `oxibrain-embed-local`

New crate. Multilingual encoder behind `EmbeddingPort` (§8.2). Separate from
`oxibrain-llm-local`: different model lifecycle, and retrieval-only deployments need embeddings
without inference.

### 7.4 — `TokenizerPort` and exact budgets

`oxibrain-ports/src/tokenizer.rs`: the trait (§7.5).

`oxibrain-core/src/context.rs`: rename `estimate_tokens` → `estimate_tokens_rough` and document
it as the pre-load fallback only. Every caller in `store/context.rs` takes a `&dyn TokenizerPort`.

**Test:** for each parity-corpus language, `count()` of a fixed 500-token passage is within 5% of
500. Today the `chars/4` heuristic is off by roughly fivefold on CJK (F27).

### 7.5 — Model artifacts

`crates/oxibrain/src/models.rs` plus `oxibrain model {list,pull,verify,use}` (§16.4).

- Manifest at `~/.oxi/models/manifest.toml`: role, name, url, blake3 digest, size, license.
- `oxibrain init` fetches the default set with **resumable, progress-reporting** download. This
  is the first thing a new user sees.
- `verify` re-hashes; `doctor` calls it.
- `--model-path` for air-gapped installs.
- Digest feeds `ExtractorId` (§9.5). **Changing weights must change the extractor id** — verify
  with a test, because a silent quality change is exactly what §9.5 exists to prevent.

### 7.6 / 7.7 — Dense path and quantization

- `index_ops.rs`: compute and `upsert_vector` entity/statement embeddings during projection
  (F17).
- `query.rs::semantic_search`: embed the query, KNN via `sqlite-vec`, **remove the fallthrough
  comment** (F16). If no embedder is configured, return an explicit `Err` or an empty result —
  never a silent TF-IDF substitute.
- `oxibrain-index::quantize`: binary quantization, pack 8 bits per byte, Hamming via XOR +
  popcount (D25). Pure, property-tested against the float cosine ordering on a fixture.

### 7.13 — Parity corpus

`eval/parity/{en,es,ko,ja,zh,ar,th}/`, ~20 episodes each, covering the seven properties in §7.8.

Each episode carries annotations: entities with byte spans, expected statements, and 2–3
questions with reference answers.

**The corpus is organized by property, not by language** — the directory is a convenience, the
manifest maps each language to the properties it exercises. That is what makes the gate mean
something for languages we never added.

`oxibrain eval --suite parity` computes per-property metrics and the cross-property variance
(§17.3). Wire it as a CI gate at 10pp.

---

## 4. Test plan

Beyond each task's own tests:

| Test | Asserts |
|---|---|
| `reproject_determinism` (existing, renamed target) | truth half byte-identical |
| `ranking_equivalence` (new) | ranking half equivalent across CPU/Metal within the measured tolerance |
| `migration_v5_to_v6` | v5 fixture opens, migrates, reprojects; both FTS tables populated |
| `no_language_tables` | grep-style CI test enforcing §18 rule 6 |
| `tokenizer_parity` | `count()` within 5% for every parity language |
| `grammar_schema_agreement` | GBNF and JSON Schema accept the same language |
| `model_digest_changes_extractor_id` | swapping weights changes `ExtractorId` |
| `no_api_key_e2e` | `init` → `ingest` → `ask` with the network disabled after `init` |
| `local_inference_off_writer` | a slow local generation does not raise read p95 beyond §16.3 |

---

## 5. Acceptance

M7 exits when every checkbox in `doc/ROADMAP.md` §2.2 passes **and**:

- [ ] `doc/ARCHITECTURE.md` §5.1 contains the **measured** ranking tolerance, dated.
- [ ] `doc/ARCHITECTURE.md` §16.3 contains a local-extraction latency row with a real number.
- [ ] `doc/adr/ADR-003-local-inference-engine.md` records the spike outcome.
- [ ] F14, F15, F16, F17, F18, F22, F23, F24, F25, F26, F27, F28 are struck from §21.

---

## 6. Notes for the implementing session

- **`AGENTS.md` is binding**: no `unwrap` outside tests, typed errors across crate boundaries,
  `Timestamp` never a bare `i64`, English in source and commits.
- **Do not touch `oxibrain-store/src/query.rs::hybrid_query` beyond adding the second FTS
  channel.** Its rewrite is M8 and mixing the two makes both unreviewable.
- **The determinism split (7.8) lands before the vector writer (7.6).** Reversing that order
  means a red test suite for the middle of the milestone, and a red suite is where determinism
  regressions hide.
- If the spike (§2) goes badly, **stop and escalate** rather than working around it. D28 is
  load-bearing for C2, and quietly degrading to schema-and-repair would leave the product
  claiming something it does not do.
