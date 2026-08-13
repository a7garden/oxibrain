# Handoff — M7 Language Primitives Complete (7.8–7.12)

> **Status:** All pure-Rust M7 tasks shipped. Model-dependent tasks (7.1–7.7,
> 7.13) remain — blocked on the spike (§2) which needs `llama-cpp-2` + GGUF
> model download.
> **Branch:** `main`
> **Predecessor:** `2026-08-12-l-subproject.md`
> **Tests:** 269 pass, 0 fail. Clippy clean. Fmt clean. Standalone verified.
> **Spec:** `doc/spec/M7-model-and-language.md` · **Roadmap:** `doc/ROADMAP.md` §2

---

## What shipped this session

### M7 Language Primitives ✅ (5/12 M7 tasks + verification)

| Task | Content | Commit | Findings closed |
|---|---|---|---|
| 7.9 | `oxibrain-index::ngram` — shingles, jaccard, entropy, minhash, lsh | `4fc339a` | — |
| 7.12 | Name similarity: Jaro-Winkler → n-gram Jaccard; dependency flip | `5639502` | F14, F28 |
| 7.11 | Fallback vectors: word tokenizer → n-gram shingles; semantic→lexical-vector | `bdb3e68` | F23, F24 |
| 7.10 | Migration v6: dual FTS (unicode61 + trigram); porter removed | `aeafe4a` | F22 |
| 7.8 | Truth/Ranking split: snapshot_truth + snapshot_ranking | `71c57fa` | F18 (structural) |
| — | `no_language_tables` CI test (§18 rule 6 enforcement) | `aa6dcd8` | — |
| — | Findings struck from §21; ARCHITECTURE.md updated | `09862e3` | — |

Also: doc restructuring commit `9845303` (DESIGN.md → ARCHITECTURE.md rename,
ROADMAP + M7 spec added — landed at session start from prior sessions' work).

---

## Key architectural changes

### 1. Dependency direction flip (§18 rule 1)

**Before:** `oxibrain-index` depended on `oxibrain-core` (for `Direction`,
`PredicateFilter`).

**After:** `oxibrain-core` depends on `oxibrain-index` (as the architecture
intends). `Direction` and `PredicateFilter` moved to
`crates/oxibrain-index/src/spec.rs`. Core re-exports them from
`retrieval.rs` so existing `oxibrain_core::retrieval::Direction` paths work.

**Why:** n-gram primitives live in index (§7.3, §18 layout), but
resolution.rs (which consumes them) lives in core. The architecture says
"core may depend on ports and index" (§18 rule 1). The flip removes the
circular dependency that would otherwise block the n-gram import.

**Cargo.toml changes:**
- `oxibrain-index/Cargo.toml`: removed `oxibrain-core` + `oxibrain-ports` deps (index is now standalone: only `serde`)
- `oxibrain-core/Cargo.toml`: added `oxibrain-index`, removed `strsim`
- Workspace `Cargo.toml`: removed `strsim` from `[workspace.dependencies]`

### 2. n-gram Jaccard replaces Jaro-Winkler (§7.7, D30, F28)

`crates/oxibrain-core/src/resolution.rs`:

- `ResolutionConfig`: `w_alias` + `w_jw` deleted; `w_ngram: f64` +
  `w_embedding: PerType<f64>` added
- `score()` uses `oxibrain_index::ngram::{shingles, jaccard}` over 3-gram
  shingles of normalized surfaces
- `resolve()` takes an `embedding_sim: impl Fn(&EntityId) -> f64` closure
  (zero until M7.3/M9 — caller decides, never hardcoded in this function)
- Thresholds recalibrated: `tau_high` 0.85→0.75, `tau_low` 0.55→0.25
  (Jaccard gives lower absolute scores than Jaro-Winkler)
- `PerType<T>` type added (per-entity-type value with default + overrides)

**Caller update:** `crates/oxibrain-store/src/project.rs:101-108` — passes
`|_| 0.0` for both graph_context and embedding_sim.

### 3. n-gram fallback vectors (§7.3, F23, F24)

`crates/oxibrain-index/src/vector.rs`:

- `STOP_WORDS` (52-word English list) deleted
- `s.len() > 1` byte filter deleted (dropped all single-byte CJK tokens)
- `tokenize()` → `features()`: returns 3-gram shingles via `ngram::shingles`
- Hashing trick + fixed dimensionality preserved (determinism unchanged)

### 4. Rename: semantic → lexical-vector (F16 context)

- `QueryMode::Semantic` → `QueryMode::LexicalVector`
- `semantic_search()` → `lexical_vector_search()`
- MCP API string: `"semantic"` → `"lexical-vector"`
- Calling a hashed bag-of-shingles "semantic" is how F16 survived review

### 5. Dual FTS index (§7.4, F22)

`crates/oxibrain-store/src/migrations/v6.sql`:
- `DROP TABLE IF EXISTS episodes_fts;`
- `CREATE VIRTUAL TABLE fts_word USING fts5(body, ..., tokenize='unicode61')`
- `CREATE VIRTUAL TABLE fts_ngram USING fts5(body, ..., tokenize='trigram')`

`query.rs::fts_search` gains `FtsIndex::Word | FtsIndex::Ngram` parameter.
`hybrid_query` queries both indexes; each enters RRF as a separate channel.
`LEDGER_SCHEMA_VERSION` = 6.

### 6. Truth/Ranking snapshot split (§5.1, P1, F18)

`crates/oxibrain-store/src/index_ops.rs`:

- `snapshot_truth(conn, space)` — byte-identical; covers entities, entity_keys,
  entity_merges, statements, assertions, mentions, beliefs, predicates.
  **Excludes** vectors, FTS, salience (all ranking-half).
- `snapshot_ranking(conn, space)` — equivalent contract; covers fts_word,
  fts_ngram, tfidf_vectors, communities, salience. Currently deterministic
  (no float embeddings yet). Tolerance calibration deferred until 7.3/7.7.
- Old `snapshot_indexes` removed. Facade exposes `snapshot_truth` +
  `snapshot_ranking`.

**Tolerance calibration still needed** (spec §3, 7.8): after 7.3 (embeddings)
and 7.7 (quantization) land, measure recall@10 on the probe set on CPU and
Metal, set tolerance to `max(2pp, 2 × observed_max_delta)`, and write the
measured number + date into ARCHITECTURE.md §5.1.

---

## Architectural state (delta from last handoff)

```
crates/oxibrain-index/src/
├── ngram.rs              NEW — shingles, jaccard, entropy, minhash, lsh (7.9)
├── spec.rs               NEW — Direction, PredicateFilter (moved from core)
├── lib.rs                +ngram/spec exports, -core/ports deps
├── vector.rs             tokenize()→features() with n-gram shingles (7.11)
├── Cargo.toml            -oxibrain-core, -oxibrain-ports (standalone)

crates/oxibrain-core/src/
├── resolution.rs         n-gram Jaccard, PerType, w_embedding (7.12)
├── retrieval.rs          Semantic→LexicalVector, Direction/PredicateFilter re-export
├── Cargo.toml            +oxibrain-index, -strsim

crates/oxibrain-store/src/
├── index_ops.rs          snapshot_truth + snapshot_ranking (7.8); rebuild_fts dual (7.10)
├── query.rs              fts_search(FtsIndex), lexical_vector_search, hybrid dual-FTS
├── migration.rs          v6 migration block
├── schema.rs             LEDGER_SCHEMA_VERSION 5→6
├── project.rs            resolve() +embedding_sim param
└── migrations/v6.sql     NEW — DROP episodes_fts; CREATE fts_word + fts_ngram

crates/oxibrain-mcp/src/server.rs   "semantic"→"lexical-vector" in mode enum + schema

crates/oxibrain/
├── src/lib.rs            snapshot_truth + snapshot_ranking (replaced snapshot_indexes)
├── src/compat.rs         updated reference checks
├── tests/no_language_tables.rs   NEW — §18 rule 6 CI enforcement
└── tests/m2_index_determinism.rs updated to use truth/ranking split
```

---

## Findings status

| Finding | Status | Closed by |
|---|---|---|
| F14 `w_alias` dead field | ✅ closed | 7.12 |
| F15 no EmbeddingPort impl | ⏳ open | 7.3 |
| F16 dense search branch is a comment | ⏳ open | 7.6 |
| F17 upsert_vector no production caller | ⏳ open | 7.6 |
| F18 vectors in byte-identical snapshot | ✅ structural split | 7.8 (tolerance pending 7.3) |
| F22 FTS porter stemmer | ✅ closed | 7.10 |
| F23 English stopword list | ✅ closed | 7.11 |
| F24 byte-length filter | ✅ closed | 7.11 |
| F25 Chinese sentence → 1 token | ⏳ partially (trigram FTS helps; tokenizer pending) | 7.4/7.10 |
| F26 Korean agglutinated particles | ⏳ open | 7.4 |
| F27 estimate_tokens = chars/4 | ⏳ open | 7.4 |
| F28 Jaro-Winkler prefix bonus | ✅ closed | 7.12 |

---

## Verification snapshot

```
$ cargo test --workspace
269 passed, 0 failed

$ cargo clippy --workspace --all-targets -- -D warnings
Finished (clean)

$ cargo fmt --all -- --check
(clean)

$ cargo build -p oxibrain --no-default-features --features http-llm
Finished (standalone build)

$ cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'
CLEAN: no oxi-ecosystem deps

$ cargo test -p oxibrain --test no_language_tables
1 passed (§18 rule 6 enforced)
```

---

## Next session: M7 model tasks (7.1–7.7, 7.13)

### Prerequisite: the spike (§2) — do this FIRST

`doc/spec/M7-model-and-language.md` §2 says:

> Do this before anything else. It is one day and it can invalidate 7.1–7.2.

1. Take `oxibrain_core::extraction::schema_from_registry` output
2. Convert to GBNF
3. Run a quantized multilingual instruct model via `llama-cpp-2` over **ten
   golden-corpus episodes** (five English, five non-Latin-script)
4. Record: parse-failure rate (must be 0), validator-rejection rate,
   wall-clock per episode, peak RSS

**Problem:** no golden corpus exists yet (`eval/` directory is absent).
The spike needs test episodes. Options:
- Create minimal golden-corpus episodes as part of the spike
- Use the extraction test fixtures in `crates/oxibrain-core/src/extraction.rs`

**Decision table** (from spec §2):

| Result | Action |
|---|---|
| 0 parse failures, validator rejections comparable to HTTP | Proceed. `llama-cpp-2` is the engine |
| 0 parse failures, rejections much worse | Proceed; treat extraction quality as M7→gate risk |
| Grammar wiring impractical | Re-evaluate `candle`/`mistral.rs`; if none works, **escalate** — D28 is load-bearing |

Record outcome in `doc/adr/ADR-003-local-inference-engine.md`.

### Task ordering (spec §3 — tree stays green, each = 1 commit)

The pure tasks (7.8–7.12) are done. Remaining, in order:

1. **7.1** — `oxibrain-llm-local` crate: GGUF inference (Metal/CUDA/CPU) behind `LlmPort`
2. **7.2** — `grammar_from_registry()` GBNF + `LlmPort::generate_constrained()`
3. **7.3** — `oxibrain-embed-local` crate: multilingual encoder behind `EmbeddingPort`
4. **7.4** — `TokenizerPort` trait; `estimate_tokens` → `estimate_tokens_rough`
5. **7.5** — Model artifacts: manifest, fetch/pin/verify, `oxibrain model` CLI
6. **7.6** — Wire `upsert_vector` into projection; dense `semantic_search` path (F16, F17)
7. **7.7** — Binary quantization (Hamming via XOR+popcount)
8. **7.13** — Parity corpus: 7 languages × 7 properties, ~20 episodes each

**7.2 is load-bearing** (D28): without grammar-constrained decoding, a small
local model is not a viable extractor, and C2 collapses.

### After model tasks: tolerance calibration (7.8 follow-up)

Once 7.3 + 7.7 land:
- Create `eval/probes/` with a fixed probe set
- Measure recall@10 on CPU and Metal, ten runs each
- Set tolerance to `max(2pp, 2 × observed_max_delta)`
- Write the measured number + date into ARCHITECTURE.md §5.1
- Update `ranking_equivalence` test to use the tolerance

### Then: the gate (ROADMAP §4)

Three-arm comparison (full context vs lexical+dense+RRF vs oxibrain complete)
under both extractors (local + frontier), on LongMemEval + golden corpus.
The reported quantity is (c) − (b) per category. Pre-committed responses in
ROADMAP §4.

---

## Critical context for the next session

- **Dependency direction:** core → index (not reverse). New index algorithms
  are available to core via `oxibrain_index::*`. If you need to use an index
  function from core, it just works now.
- **`PerType<T>`** lives in `crates/oxibrain-core/src/resolution.rs`. It's
  not re-exported at the crate root — access via `oxibrain_core::resolution::PerType`.
- **`FtsIndex`** enum lives in `crates/oxibrain-store/src/query.rs`. Not
  exported publicly.
- **Migration v6** drops `episodes_fts` and creates `fts_word` + `fts_ngram`.
  The v3.sql still creates the old table (historical, immutable) — v6 drops
  it. Do not modify v3.sql.
- **`snapshot_truth` queries:** predicates table is global (no space_id). The
  predicates snapshot uses `snapshot_query_global()` (no params). All other
  truth tables are space-scoped via subqueries.
- **Test count:** 269 (was 237 at M6 exit; +28 from Q/R/L; +4 from M7
  language primitives; +1 from no_language_tables).
- **No `eval/` directory exists.** The spike and 7.13 need it created.
- **`llama-cpp-2`** has not been added to the workspace yet. It requires C++
  build tooling. Check `candle` / `mistral.rs` as alternatives if the build
  is problematic.
- **Branch:** `main`. All commits this session were direct.
