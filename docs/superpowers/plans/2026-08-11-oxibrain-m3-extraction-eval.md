# oxibrain M3 — Extraction & Evaluation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan
> task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the extraction and evaluation layer — job queue, LLM port + HTTP
adapter, registry-generated JSON Schema, extraction pipeline (off-actor LLM,
cached responses), validator with repair/quarantine, verbatim mention capture,
extractor identity + re-extraction, consolidation + community summaries (cached
`Derived` episodes), confidence calibration, golden corpus + eval harness, and
budget measurement.

**Architecture:** New `oxibrain-llm-http` crate (Anthropic/OpenAI adapters
behind `LlmPort`). Core gains `extraction.rs` (types + pure schema/validator/prompt
fns) and `confidence.rs`. Store gains `extraction.rs` (job queue + cache +
project), `quarantine.rs`, `consolidation.rs`. Brain gains `Option<Arc<dyn
LlmPort>>` and extraction/consolidation methods. The critical constraint: LLM
calls run off-actor (async, in Brain facade), DB writes are short WriteOps.

**Tech Stack:** Rust 2024, rusqlite (existing tables), serde_json, reqwest
(HTTP adapter), async-trait, tokio.

**Spec:** `docs/superpowers/specs/2026-08-11-oxibrain-m3-extraction-eval-design.md`

## Global Constraints

- Rust 2024 edition, MSRV 1.85.
- `clippy --all-targets --all-features -- -D warnings` clean.
- `#![cfg_attr(test, allow(clippy::unwrap_used))]` in every crate root.
- Timestamp API: `Timestamp::from_millis(i64)` / `Timestamp::millis() -> i64`.
  NEVER use `.as_i64()`.
- rusqlite errors → `crate::sql_err(e)?` (the store-local helper). NEVER `?` on
  rusqlite directly (orphan rule blocks auto-conversion).
- Only `oxibrain-store` may reference `rusqlite`. Core and llm-http are pure/adapter.
- Content-derived ids; no randomness in anything persisted.
- Space is passed as the content-derived ID (from `ensure_space`), not the name.
- **No LLM call inside a database transaction (§7.2).** LLM calls live in the
  Brain facade (async). Store functions are synchronous DB-only.
- Default features pull zero oxi-ecosystem crates and zero HTTP deps.
- Comments and commit messages in English.

---

## File Structure

```
crates/
├── oxibrain-ports/src/
│   ├── llm.rs               # EXTEND — already sufficient (LlmRequest/LlmResponse/LlmPort)
│   └── llm_fake.rs          # NEW — FakeLlmPort for tests
├── oxibrain-core/src/
│   ├── extraction.rs        # NEW — Claim, MentionRef, ExtractionResponse, ClaimObject,
│   │                        #         ExtractorConfig, ExtractMechanism, ExtractorId,
│   │                        #         schema_from_registry, build_extraction_prompt,
│   │                        #         validate_claims, ValidationResult, ValidationError
│   ├── confidence.rs        # NEW — ConfidenceComponents, CalibrationTable, calibrate
│   └── lib.rs               # EXTEND — pub mod extraction, confidence
├── oxibrain-llm-http/       # NEW crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs           # re-exports
│       ├── anthropic.rs     # Anthropic adapter (tool-use structured output)
│       └── openai.rs        # OpenAI adapter (json_schema structured output)
├── oxibrain-store/src/
│   ├── extraction.rs        # NEW — IngestJob, JobState, job queue CRUD, cache CRUD,
│   │                        #         project_extraction, project_from_cache
│   ├── quarantine.rs        # NEW — ExtractionFailure, record_failure, list_failures
│   ├── consolidation.rs     # NEW — EpisodeCluster, CommunityGroup, store primitives
│   ├── project.rs           # EXTEND — resolve_or_create → pub(crate)
│   ├── reproject.rs         # EXTEND — replay extractions from cache (step 4)
│   └── lib.rs               # EXTEND — pub mod extraction, quarantine, consolidation
├── oxibrain/src/
│   ├── lib.rs               # EXTEND — Brain gains Option<Arc<dyn LlmPort>>;
│   │                        #   ingest, extract_pending, extract_one, reextract,
│   │                        #   consolidate, summarize_communities, job_status
│   └── config.rs            # EXTEND — llm config
├── oxibrain-cli/src/
│   └── cmd/
│       ├── extract.rs       # NEW — `oxibrain extract`
│       ├── reextract.rs     # NEW — `oxibrain reextract`
│       └── eval.rs          # NEW — `oxibrain eval`
├── eval/                    # NEW — golden corpus + eval harness
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs           # EvalRunner
│       ├── metrics.rs       # precision, recall, F1
│       └── corpus/          # JSON fixtures
└── Cargo.toml               # EXTEND — add oxibrain-llm-http, reqwest, eval workspace members
```

---

## Sub-Session M3a: Job Queue + LLM Port + Extractor Identity

### Task 1: Core extraction types + extractor identity

**Files:**
- Create: `crates/oxibrain-core/src/extraction.rs`
- Modify: `crates/oxibrain-core/src/lib.rs`

**Interfaces:**
- Produces: `Claim`, `MentionRef`, `ClaimObject`, `ExtractionResponse`, `Polarity`
  (re-export from knowledge), `ExtractorConfig`, `ExtractMechanism`,
  `ExtractorConfig::id()`, `ValidationResult`, `ValidationError`, `ExtractSummary`,
  `ExtractionBudget`
- Consumes: `oxibrain_core::registry::{PredicateDef, ObjectKind, LiteralType}`,
  `oxibrain_core::knowledge::Polarity`

- [ ] **Step 1: Create extraction.rs with type definitions**

Define all types from spec §5.1. Key points:
- `Polarity` — re-export `oxibrain_core::knowledge::Polarity` (already exists with `Affirm`/`Deny`). Do NOT redefine.
- `ExtractorConfig` — fields: `model_id: String`, `prompt_version: u32`, `registry_major: u32`, `mechanism: ExtractMechanism`, `max_tokens: u32`. Method `id(&self) -> String` using blake3.
- `ExtractMechanism` — enum: `JsonSchema`, `ToolCall`, `JsonMode`. Derive `Serialize, Deserialize, Clone, Copy, PartialEq, Eq`. `#[serde(rename_all = "snake_case")]`.
- `MentionRef` — fields: `surface: String`, `entity_type: String`, `span: (u32, u32)`.
- `ClaimObject` — tagged enum: `Entity { mention: MentionRef }`, `Literal { literal_type: String, value: String, span: (u32, u32) }`.
- `Claim` — fields: `predicate: String`, `subject: MentionRef`, `object: ClaimObject`, `polarity: Polarity`, `valid_from: Option<i64>`, `valid_to: Option<i64>`, `confidence: f32`.
- `ExtractionResponse` — `claims: Vec<Claim>`.
- `ValidationResult` — `valid: Vec<Claim>`, `invalid: Vec<(Claim, Vec<ValidationError>)>`.
- `ValidationError` — tagged enum per spec §5.1.
- `ExtractSummary` — `extracted: usize`, `quarantined: usize`, `episodes_done: usize`, `episodes_failed: usize`.
- `ExtractionBudget` — per spec §8.3. `Default` with: max_concurrent=4, max_episodes_per_batch=50, max_tokens_per_episode=8192, max_repair_attempts=1, lease_timeout_secs=300.

- [ ] **Step 2: Add to lib.rs**

```rust
pub mod extraction;
```

Add `blake3` and `hex` to `oxibrain-core/Cargo.toml` `[dependencies]` if not already there (they are — used by id.rs).

- [ ] **Step 3: Write tests for ExtractorConfig::id()**

Test that same config → same id; different model → different id; different mechanism → different id. Test that registry_major change → different id (cache invalidation).

- [ ] **Step 4: Run tests**

```bash
cargo test -p oxibrain-core extraction
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m3): core extraction types + extractor identity"
```

---

### Task 2: Confidence types + calibration

**Files:**
- Create: `crates/oxibrain-core/src/confidence.rs`
- Modify: `crates/oxibrain-core/src/lib.rs`

**Interfaces:**
- Produces: `ConfidenceComponents`, `CalibrationTable`, `calibrate()`,
  `derive_calibration()`

- [ ] **Step 1: Create confidence.rs**

Define types from spec §5.2:
- `ConfidenceComponents` — fields: raw, calibrated, corroboration, trust, recency (all f32). Method `combine(&self) -> f32`.
- `CalibrationTable` — `values: BTreeMap<String, f32>`. Methods: `get`, `set`, `Default`.
- `calibrate(extractor_id, table) -> f32` — returns table value or 0.8 conservative prior.
- `derive_calibration(metrics: &EvalMetrics) -> f32` — placeholder that takes precision and fabrication rate.

Note: `EvalMetrics` is defined in the eval crate (Task 13). For now, `derive_calibration` takes `(precision: f64, fabrication_rate: f64) -> f32`.

- [ ] **Step 2: Write tests**

Test `combine()` clamps to [0,1]. Test `calibrate` returns prior for unknown extractor. Test `derive_calibration` is monotonic in precision.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(m3): confidence calibration types"
```

---

### Task 3: Schema generation + prompt builder (pure fns)

**Files:**
- Modify: `crates/oxibrain-core/src/extraction.rs`

**Interfaces:**
- Produces: `schema_from_registry(&[PredicateDef]) -> serde_json::Value`,
  `build_extraction_prompt(&[PredicateDef]) -> String`

- [ ] **Step 1: Implement schema_from_registry**

Pure fn per spec §6.1. Generates JSON Schema constraining:
- `claims` array of claim objects
- `predicate` enum of known predicate names
- `subject` mention schema (surface, type enum, span array)
- `object` oneOf entity/literal
- `polarity` enum ["affirm", "deny"]
- `confidence` number [0.0, 1.0]

Use `serde_json::json!` macro. Collect entity types from `predicates.iter().flat_map(|p| p.subject_types.iter())`.

- [ ] **Step 2: Implement build_extraction_prompt**

Pure fn per spec §6.2. Builds system prompt from registry descriptions + examples. No hard-coded predicates (P4).

- [ ] **Step 3: Write tests**

- `schema_from_registry(core_v1())` produces valid JSON with all predicate names in the enum.
- Same registry → same schema (deterministic).
- Prompt contains all predicate descriptions.
- Prompt contains instructions about verbatim surface + byte spans.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): registry-generated JSON Schema + extraction prompt"
```

---

### Task 4: FakeLlmPort

**Files:**
- Create: `crates/oxibrain-ports/src/llm_fake.rs`
- Modify: `crates/oxibrain-ports/src/lib.rs`

**Interfaces:**
- Produces: `FakeLlmPort` implementing `LlmPort`
- Consumes: `LlmPort`, `LlmRequest`, `LlmResponse` (from llm.rs)

- [ ] **Step 1: Implement FakeLlmPort**

```rust
use crate::llm::{LlmPort, LlmRequest, LlmResponse};
use crate::error::BrainError;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct FakeLlmPort {
    responses: Mutex<HashMap<String, LlmResponse>>,
}

impl FakeLlmPort {
    pub fn new() -> Self { Self { responses: Mutex::new(HashMap::new()) } }

    /// Register a canned response keyed by a substring of the prompt.
    pub fn respond_to(&self, key: impl Into<String>, response: LlmResponse) {
        self.responses.lock().unwrap().insert(key.into(), response);
    }
}

#[async_trait::async_trait]
impl LlmPort for FakeLlmPort {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError> {
        let map = self.responses.lock().unwrap();
        // Find first matching key (substring match on prompt).
        for (key, resp) in map.iter() {
            if req.prompt.contains(key) {
                return Ok(resp.clone());
            }
        }
        Err(BrainError::Config(format!(
            "FakeLlmPort: no canned response for prompt (tried {} keys)",
            map.len()
        )))
    }
}
```

Add `pub mod llm_fake;` to `crates/oxibrain-ports/src/lib.rs`.

- [ ] **Step 2: Write tests**

Test that FakeLlmPort returns the canned response when the key matches the prompt, and errors when no match.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(m3): FakeLlmPort for deterministic tests"
```

---

### Task 5: oxibrain-llm-http crate scaffold

**Files:**
- Create: `crates/oxibrain-llm-http/Cargo.toml`
- Create: `crates/oxibrain-llm-http/src/lib.rs`
- Create: `crates/oxibrain-llm-http/src/anthropic.rs`
- Create: `crates/oxibrain-llm-http/src/openai.rs`
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Produces: `AnthropicLlm`, `OpenAiLlm` implementing `LlmPort`

- [ ] **Step 1: Add workspace member + reqwest dep**

Modify `Cargo.toml` (workspace root):
- Add `"crates/oxibrain-llm-http"` to members
- Add `oxibrain-llm-http = { path = "crates/oxibrain-llm-http", version = "0.1.0" }` to workspace.dependencies
- Add `reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }` to workspace.dependencies

- [ ] **Step 2: Create Cargo.toml for oxibrain-llm-http**

```toml
[package]
name = "oxibrain-llm-http"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
oxibrain-ports.workspace = true
async-trait.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tracing.workspace = true
```

- [ ] **Step 3: Implement AnthropicLlm**

Uses Messages API with tool use for structured output. Constructor takes `(api_key: String, model: String)`. The `complete` method:
1. Wraps `req.json_schema` as a tool definition with `input_schema`.
2. Sets `tool_choice: { type: "tool", name: "extract_claims" }`.
3. POSTs to `https://api.anthropic.com/v1/messages`.
4. Parses the tool call arguments from the response.
5. Returns `LlmResponse { text: tool_args_json, raw: full_response }`.

Handle errors: map reqwest errors to `BrainError::Provider { retryable, .. }`. Wait — `BrainError` doesn't have a `Provider` variant yet (DESIGN §13.5 lists it but M0 only defined a subset). Add `Provider { retryable: bool, message: String }` and `Extraction(String)` and `Budget(String)` to `BrainError` in ports/error.rs.

- [ ] **Step 4: Implement OpenAiLlm**

Uses Chat Completions API with `response_format: { type: "json_schema", json_schema: { name: "extraction", schema: ..., strict: true } }`. Constructor takes `(api_key: String, model: String)`. The `complete` method:
1. POSTs to `https://api.openai.com/v1/chat/completions`.
2. Parses `choices[0].message.content` as JSON.
3. Returns `LlmResponse { text: content, raw: full_response }`.

- [ ] **Step 5: Create lib.rs re-exports**

```rust
pub mod anthropic;
pub mod openai;
pub use anthropic::AnthropicLlm;
pub use openai::OpenAiLlm;
```

- [ ] **Step 6: Extend BrainError**

Add to `crates/oxibrain-ports/src/error.rs`:
```rust
    #[error("provider error (retryable: {retryable}): {message}")]
    Provider { retryable: bool, message: String },
    #[error("extraction error: {0}")]
    Extraction(String),
    #[error("budget exceeded: {0}")]
    Budget(String),
```
Update `retryable()` to include `Provider { retryable: true, .. }`.

- [ ] **Step 7: Verify compilation**

```bash
cargo build -p oxibrain-llm-http
cargo clippy -p oxibrain-llm-http -- -D warnings
```

No network calls in tests — the adapters are thin HTTP wrappers tested via FakeLlmPort in integration tests.

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat(m3): oxibrain-llm-http crate (Anthropic + OpenAI adapters)"
```

---

### Task 6: Job queue CRUD

**Files:**
- Create: `crates/oxibrain-store/src/extraction.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`
- Modify: `crates/oxibrain-store/src/project.rs` (make `resolve_or_create` pub(crate))

**Interfaces:**
- Produces: `IngestJob`, `JobState`, `enqueue_job`, `claim_jobs`, `complete_job`,
  `fail_job`, `reclaim_expired`, `list_jobs`, `job_count_by_state`

- [ ] **Step 1: Define IngestJob + JobState**

Per spec §5.3. `JobState` enum with `as_str()` / `parse()`.

- [ ] **Step 2: Implement job queue CRUD**

All functions take `&Connection`:
- `enqueue_job(conn, episode_id, extractor_id, now) -> Result<String>` — INSERT with state='ready', id=blake3(episode_id, extractor_id).
- `claim_jobs(conn, extractor_id, lease_timeout_secs, limit, now) -> Result<Vec<IngestJob>>` — UPDATE...WHERE state='ready' SET state='leased', lease_until=now+timeout; then SELECT the leased rows.
- `complete_job(conn, job_id, now) -> Result<()>` — UPDATE state='done'.
- `fail_job(conn, job_id, error, max_attempts, now) -> Result<JobState>` — increment attempts; if >= max → state='failed', else state='ready'.
- `reclaim_expired(conn, now) -> Result<usize>` — UPDATE state='ready' WHERE state='leased' AND lease_until < now.
- `list_jobs(conn, state: Option<JobState>) -> Result<Vec<IngestJob>>`.
- `job_count_by_state(conn) -> Result<HashMap<String, usize>>`.

Use `crate::sql_err` for all rusqlite errors. Job id is deterministic: `blake3(episode_id, extractor_id)` so re-enqueueing the same episode+extractor is idempotent.

- [ ] **Step 3: Make resolve_or_create pub(crate)**

In `crates/oxibrain-store/src/project.rs`, change `fn resolve_or_create` to `pub(crate) fn resolve_or_create`. This is needed for `project_extraction` in Task 8.

- [ ] **Step 4: Add module to lib.rs**

```rust
pub mod extraction;
```

- [ ] **Step 5: Write tests**

- Enqueue + claim + complete lifecycle.
- Enqueue + claim + fail (retry → ready, then fail again → failed).
- Reclaim expired leases.
- Idempotent enqueue (same episode + extractor → same job id).
- `list_jobs` filters by state.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(m3): job queue lifecycle (claim/lease/complete/retry)"
```

---

### Task 7: Cache CRUD

**Files:**
- Modify: `crates/oxibrain-store/src/extraction.rs`

**Interfaces:**
- Produces: `cache_response`, `get_cached_response`

- [ ] **Step 1: Implement cache CRUD**

```rust
pub fn cache_response(conn: &Connection, episode_id: &str, extractor_id: &str,
                      raw_response: &str, now: Timestamp) -> Result<(), BrainError> {
    let hash = oxibrain_core::content_hash(raw_response);
    conn.execute(
        "INSERT OR REPLACE INTO extractions (episode_id, extractor_id, response_hash, raw_response, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![episode_id, extractor_id, hash.0.as_slice(), raw_response, now.millis()],
    ).map_err(sql_err)?;
    Ok(())
}

pub fn get_cached_response(conn: &Connection, episode_id: &str, extractor_id: &str)
    -> Result<Option<String>, BrainError> {
    // SELECT raw_response FROM extractions WHERE episode_id=? AND extractor_id=?
}
```

Use `INSERT OR REPLACE` because the PK is `(episode_id, extractor_id)` — re-extraction with the same extractor overwrites the cache.

Wait — spec §9.3 says re-extraction with the same extractor is a no-op (idempotency layer 2). So we should use `INSERT OR IGNORE` and check first. Actually, `INSERT OR REPLACE` is fine because re-extracting with the same extractor should produce the same response (same model, same prompt, same content). If the response differs (non-deterministic LLM), the latest wins, which is acceptable. But for determinism, we should check cache first and skip if hit. The `get_cached_response` function enables that check.

- [ ] **Step 2: Write tests**

- Cache + retrieve round-trip.
- Re-cache overwrites.
- Get non-existent returns None.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(m3): extraction response cache CRUD"
```

---

### Task 8: Brain gains LLM port + ingest/extract facades

**Files:**
- Modify: `crates/oxibrain/src/lib.rs`
- Modify: `crates/oxibrain/src/config.rs`
- Modify: `crates/oxibrain/Cargo.toml`

**Interfaces:**
- Produces: `Brain::with_llm`, `Brain::ingest`, `Brain::extract_pending`,
  `Brain::extract_one`, `Brain::job_status`

- [ ] **Step 1: Add LlmPort to Brain struct**

```rust
pub struct Brain {
    handle: Arc<StoreHandle>,
    clock: Arc<dyn ClockPort>,
    llm: Option<Arc<dyn LlmPort>>,
}
```

Update `open()` and `with_clock()` to set `llm: None`. Add `with_llm(config, clock, llm)` constructor. Add `pub fn llm(&self) -> Option<&Arc<dyn LlmPort>>`.

- [ ] **Step 2: Implement Brain::ingest**

Creates a Primary episode + enqueues an extraction job. Takes `(space, content, source, trust)`. Uses the existing WriteOp pattern. Returns episode_id.

```rust
pub async fn ingest(&self, space: &str, content: String, source: SourceRef,
                    trust: TrustTier, extractor_id: &str) -> Result<String, BrainError> {
    // WriteOp: insert episode + enqueue_job
}
```

The `extractor_id` parameter determines which extractor will process this episode. Default is the configured extractor.

- [ ] **Step 3: Implement Brain::extract_pending**

The batch extraction worker. Orchestrates off-actor LLM calls:
1. Claim jobs [WriteOp]
2. For each job: read episode [reader], generate schema [pure], build prompt [pure], call LLM [async], parse [pure], validate [pure — Task 10], repair [optional retry]
3. Project [WriteOp]: cache response + project_extraction + complete/fail job

Returns `ExtractSummary`.

This method calls `self.llm` — returns error if None.

- [ ] **Step 4: Implement Brain::extract_one**

Synchronous single-episode extraction. Same orchestration but for one episode_id. Used for testing and realtime mode.

- [ ] **Step 5: Implement Brain::job_status**

Reads job counts by state. Uses reader pool.

- [ ] **Step 6: Write integration test**

Using FakeLlmPort: ingest an episode, extract_one, verify assertions were created. This test requires `project_extraction` (Task 9) — so this step may be deferred to after Task 9. For now, test that ingest creates a job and job_status shows it as ready.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(m3): Brain gains LLM port + ingest/extract facades"
```

---

## Sub-Session M3b: Schema Generation + Extraction Pipeline + Re-extraction

### Task 9: project_extraction (claims → assertions)

**Files:**
- Modify: `crates/oxibrain-store/src/extraction.rs`

**Interfaces:**
- Produces: `project_extraction(conn, space, episode_id, extractor_id, claims, now)`
  -> Result<usize>
- Consumes: `project::resolve_or_create` (pub(crate) from Task 6),
  `knowledge::*` CRUD, `fold`, registry

- [ ] **Step 1: Implement project_extraction**

For each valid claim:
1. Resolve subject entity via `resolve_or_create(conn, space, &eref, episode_id, claim.subject.span.0, now)`.
2. Resolve object (entity or literal) — similar to `resolve_object` in project.rs but with real spans.
3. Create statement (idempotent) — `statement_id(space, &subj_id, &predicate, &object)`.
4. Create assertion — with `extractor = Some(extractor_id)`, `confidence = claim.confidence`, `claimed_from/_to` from claim (sentinels if None).
5. Capture mentions — subject mention with `span = claim.subject.span`, object mention with real span.
6. Re-fold affected group — `fold(&pred_def, &group, now)` + `replace_beliefs`.

Follow the exact pattern of `project_declaration` in project.rs:263-444. The key difference: real byte spans (not fixed 0/100/200) and extractor_id (not "declaration"/None) and confidence (not 1.0).

Use `oxibrain_core::id::TIME_MIN` / `TIME_MAX` for `None` valid_from/to.

- [ ] **Step 2: Implement project_from_cache**

Parse cached raw_response → ExtractionResponse → validate_claims → project_extraction. No LLM call. Used by reproject.

```rust
pub fn project_from_cache(conn, space, episode_id, extractor_id, raw_response, content, predicates, now) -> Result<usize> {
    let response: ExtractionResponse = serde_json::from_str(raw_response)
        .map_err(|e| BrainError::Extraction(format!("parse: {e}")))?;
    let result = oxibrain_core::extraction::validate_claims(&response.claims, content, predicates);
    project_extraction(conn, space, episode_id, extractor_id, &result.valid, now)
}
```

Note: `validate_claims` is implemented in Task 10. For now, project_extraction can be tested with pre-validated claims.

- [ ] **Step 3: Write tests**

- Project a single claim → verify assertion + mentions created with correct spans.
- Project multiple claims from one episode → verify all projected.
- Idempotency: project the same claims twice → no duplicate assertions (content-derived IDs).
- Verify beliefs re-folded correctly.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): project_extraction — claims to assertions"
```

---

### Task 10: Validator (validate_claims)

**Files:**
- Modify: `crates/oxibrain-core/src/extraction.rs`

**Interfaces:**
- Produces: `validate_claims(&[Claim], content: &str, &[PredicateDef]) -> ValidationResult`,
  helper fns `check_span`, `check_verbatim`

- [ ] **Step 1: Implement validate_claims**

Per spec §7.1. For each claim:
1. Check confidence in [0, 1].
2. Find predicate in registry → reject if unknown.
3. Check subject type ∈ predicate.subject_types.
4. Check object type matches predicate.object_kind (Entity vs Literal, type match).
5. `check_span` — span bounds within content length.
6. `check_verbatim` — surface form matches content at span (the fabricated-entity gate).

Partition into valid/invalid.

- [ ] **Step 2: Implement helper functions**

```rust
fn check_span(span: (u32, u32), content: &str, errors: &mut Vec<ValidationError>) {
    let len = content.len();
    if span.0 as usize >= len || span.1 as usize > len || span.0 >= span.1 {
        errors.push(ValidationError::SpanOutOfBounds { span, content_len: len });
    }
}

fn check_verbatim(m: &MentionRef, content: &str, errors: &mut Vec<ValidationError>) {
    let bytes = content.as_bytes();
    if m.span.0 as usize >= bytes.len() || m.span.1 as usize > bytes.len() { return; }
    let found = &content[m.span.0 as usize..m.span.1 as usize];
    if found != m.surface {
        errors.push(ValidationError::SurfaceNotVerbatim {
            surface: m.surface.clone(), span: m.span, found: found.to_string(),
        });
    }
}

fn literal_type_matches(given: &str, expected: &LiteralType) -> bool {
    match (given, expected) {
        ("text", LiteralType::Text) => true,
        ("date", LiteralType::Date) => true,
        ("datetime", LiteralType::DateTime) => true,
        ("number", LiteralType::Number) => true,
        ("bool", LiteralType::Bool) => true,
        ("quantity", LiteralType::Quantity { .. }) => true,
        _ => false,
    }
}
```

- [ ] **Step 3: Write property tests**

- Valid claim (correct types, verbatim surface, in-bounds span) → valid.
- Unknown predicate → invalid with UnknownPredicate.
- Type mismatch → invalid with SubjectTypeMismatch / ObjectTypeMismatch.
- Span out of bounds → invalid with SpanOutOfBounds.
- Surface not verbatim (fabricated entity) → invalid with SurfaceNotVerbatim.
- Confidence out of range → invalid with ConfidenceOutOfRange.
- Mixed: some valid, some invalid → correctly partitioned.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): validate_claims — registry-driven validator"
```

---

### Task 11: Extraction pipeline integration + repair loop

**Files:**
- Modify: `crates/oxibrain/src/lib.rs` (complete extract_pending/extract_one)

**Interfaces:**
- Produces: complete `Brain::extract_pending` and `Brain::extract_one` with
  LLM call → cache → parse → validate → repair → project

- [ ] **Step 1: Complete extract_one**

Full orchestration:
```rust
pub async fn extract_one(&self, space: &str, episode_id: &str, config: &ExtractorConfig)
    -> Result<ExtractSummary, BrainError>
{
    let llm = self.llm.as_ref().ok_or_else(|| BrainError::Config("no LLM port".into()))?;
    let now = self.clock.now();

    // 1. Read episode content [reader]
    let episode = self.get_episode(episode_id).await?
        .ok_or_else(|| BrainError::NotFound(format!("episode {episode_id}")))?;

    // 2. Generate schema + prompt [pure]
    let predicates = oxibrain_core::registry::core_v1();
    let schema = oxibrain_core::extraction::schema_from_registry(predicates);
    let system = oxibrain_core::extraction::build_extraction_prompt(predicates);

    // 3. Call LLM [async]
    let req = LlmRequest {
        model: config.model_id.clone(),
        system: Some(system),
        prompt: episode.content.clone(),
        json_schema: Some(schema),
        max_tokens: config.max_tokens,
    };
    let response = llm.complete(req).await?;

    // 4. Parse + validate [pure]
    let parsed: ExtractionResponse = serde_json::from_str(&response.text)
        .map_err(|e| BrainError::Extraction(format!("parse: {e}")))?;
    let mut result = validate_claims(&parsed.claims, &episode.content, predicates);

    // 5. Repair loop (one retry if invalid claims exist)
    if !result.invalid.is_empty() && /* attempts < max */ {
        let repair_prompt = format!("{}\n\nPrevious errors: {:?}",
            episode.content, result.invalid.iter().map(|(_, e)| e).collect::<Vec<_>>());
        let repair_req = LlmRequest { prompt: repair_prompt, ..req.clone() };
        let repair_response = llm.complete(repair_req).await?;
        let repair_parsed: ExtractionResponse = serde_json::from_str(&repair_response.text)?;
        result = validate_claims(&repair_parsed.claims, &episode.content, predicates);
        // Use the repair response for caching
    }

    // 6. Project [WriteOp]
    let h = self.handle.clone();
    let raw = response.text.clone();
    let valid = result.valid.clone();
    let invalid = result.invalid.clone();
    let extractor_id = config.id();
    tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        h.writer.submit(Box::new(move |conn| {
            // Cache response
            extraction::cache_response(conn, episode_id, &extractor_id, &raw, now)?;
            // Project valid claims
            let n = extraction::project_extraction(conn, space, episode_id, &extractor_id, &valid, now)?;
            // File invalid claims
            for (claim, errors) in &invalid {
                quarantine::record_failure(conn, episode_id, &extractor_id, &raw,
                    &serde_json::to_string(errors)?, now)?;
            }
            // Complete job
            extraction::complete_job(conn, &job_id, now)?;
            let _ = tx.send(ExtractSummary { extracted: n, quarantined: invalid.len(),
                episodes_done: 1, episodes_failed: 0 });
            Ok(())
        }))?;
        h.writer.flush()?;
        rx.recv().map_err(|_| BrainError::Storage("extract_one channel dropped".into()))
    }).await.map_err(|e| BrainError::Storage(format!("join: {e}")))?
}
```

- [ ] **Step 2: Complete extract_pending (batch version)**

Claims N jobs, processes each via the same pipeline. Uses `claim_jobs` + loop over `extract_one`-like logic.

- [ ] **Step 3: Write integration test**

Using FakeLlmPort with a canned ExtractionResponse:
1. Ingest an episode with known content.
2. Extract with FakeLlmPort (canned response with valid claims).
3. Verify assertions created with correct entities, predicates, spans.
4. Verify beliefs folded correctly.
5. Verify FTS5 indexes updated (query for the episode text).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): extraction pipeline with LLM call + repair loop"
```

---

### Task 12: Re-extraction + reproject extension

**Files:**
- Modify: `crates/oxibrain/src/lib.rs` (add `reextract`)
- Modify: `crates/oxibrain-store/src/reproject.rs` (add extraction replay)

**Interfaces:**
- Produces: `Brain::reextract`, extended `reproject()`

- [ ] **Step 1: Implement Brain::reextract**

For each Primary episode in the space:
1. Check cache: `get_cached_response(episode_id, config.id())`. If hit, skip.
2. If miss: enqueue a job with the new extractor_id, then extract.

Both old and new assertions coexist (different extractor_id).

- [ ] **Step 2: Extend reproject to replay extractions**

Add step 4 to `reproject()` (between declaration replay and index rebuild):

```rust
// 4. Replay extractions from cache (deterministic — no LLM).
let mut ext_stmt = conn.prepare(
    "SELECT e.id, e.space_id, e.content, e.ingested_at, x.extractor_id, x.raw_response
     FROM extractions x
     JOIN episodes e ON x.episode_id = e.id
     WHERE e.kind = 'primary'
     ORDER BY e.seq ASC, x.extractor_id ASC"
)?;
let extractions: Vec<(String, String, String, i64, String, String)> = ext_stmt
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
    .map_err(sql_err)?.collect::<Result<Vec<_>, _>>().map_err(sql_err)?;
drop(ext_stmt);

let predicates = oxibrain_core::registry::core_v1();
for (_ep_id, space, content, ingested_at, extractor_id, raw) in &extractions {
    crate::extraction::project_from_cache(
        conn, space, _ep_id, extractor_id, raw, content,
        predicates, Timestamp(*ingested_at)
    )?;
}
```

This must run AFTER declaration replay (step 3) because extraction may create entities that declarations reference, and declarations must be processed first for correct seq ordering. Actually — declarations and extractions process the same episodes in seq order. The canonical order is `(episode.seq, extractor_id)`. Declarations create Declaration episodes; extractions process Primary episodes. They don't overlap. But entity resolution may be affected by order. For safety, process in seq order across all episodes.

Wait — reproject currently only replays Declaration episodes. Extractions replay Primary episodes. They're disjoint sets. The order between them matters for entity resolution: if a declaration creates entity X, and an extraction mentions X, the extraction should find X. Since declarations are user-authored (trusted, seq assigned at declare time) and extractions process primary episodes (seq assigned at ingest time), the seq order handles this correctly as long as we process in seq order.

Actually, the simplest approach: process ALL projection-producing episodes (both Declaration and Primary-with-extraction) in seq order. But declarations and extractions have different projection paths. Let me keep them separate: first declarations (existing step 3), then extractions (new step 4). This works because declarations don't depend on extraction output (they're independent user assertions), and extractions may reference entities created by declarations (processed first).

- [ ] **Step 3: Extend the byte-identical reprojection test**

The test in `crates/oxibrain/tests/reproject_determinism.rs`:
1. Create a brain with declarations + primary episodes.
2. Extract primary episodes using FakeLlmPort (deterministic).
3. Snapshot projection.
4. reproject().
5. Snapshot again.
6. Assert byte-identical (now including extraction-produced assertions + mentions).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): re-extraction + reproject extraction replay"
```

---

## Sub-Session M3c: Validator + Quarantine + Consolidation

### Task 13: Quarantine store functions

**Files:**
- Create: `crates/oxibrain-store/src/quarantine.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`

**Interfaces:**
- Produces: `ExtractionFailure`, `record_failure`, `list_failures`, `retry_failure`

- [ ] **Step 1: Implement quarantine CRUD**

```rust
pub struct ExtractionFailure {
    pub id: i64,
    pub episode_id: String,
    pub extractor_id: String,
    pub raw_response: String,
    pub errors_json: String,
    pub created_at: Timestamp,
}

pub fn record_failure(conn, episode_id, extractor_id, raw_response, errors_json, now) -> Result<i64>;
pub fn list_failures(conn, space: Option<&str>) -> Result<Vec<ExtractionFailure>>;
pub fn retry_failure(conn, failure_id, now) -> Result<()>;
```

`list_failures` joins through episodes to filter by space (since extraction_failures doesn't have a space_id column — it has episode_id which joins to episodes.space_id).

- [ ] **Step 2: Write tests**

- Record + list + retry lifecycle.
- Filter by space.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(m3): quarantine — extraction failure recording"
```

---

### Task 14: Consolidation store primitives

**Files:**
- Create: `crates/oxibrain-store/src/consolidation.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`

**Interfaces:**
- Produces: `EpisodeCluster`, `CommunityGroup`, `find_episode_clusters`,
  `get_cached_summary`, `cache_summary`, `write_derived_episode`,
  `load_community_entities`

- [ ] **Step 1: Define types**

```rust
pub struct EpisodeCluster {
    pub episode_ids: Vec<String>,
    pub shared_entities: Vec<String>,
}

pub struct CommunityGroup {
    pub label: u64,
    pub entity_ids: Vec<String>,
}
```

- [ ] **Step 2: Implement find_episode_clusters**

SQL: find groups of episodes that share ≥ 2 entities (via statements), within a configurable time window. Uses a self-join on assertions grouped by episode pairs. Deterministic: sort by episode_ids for stable member_set_hash.

- [ ] **Step 3: Implement summary cache CRUD**

`get_cached_summary` / `cache_summary` operate on the `summaries` table:
```sql
SELECT text FROM summaries WHERE scope_kind=? AND member_set_hash=? AND extractor_id=?
INSERT OR REPLACE INTO summaries (scope_kind, member_set_hash, extractor_id, text, created_at) VALUES (...)
```

`member_set_hash` = blake3 of sorted episode_ids (for consolidation) or sorted entity_ids (for community). Deterministic.

- [ ] **Step 4: Implement write_derived_episode**

Creates a Derived episode + episode_links:
```rust
pub fn write_derived_episode(conn, space, text, sources, config, now) -> Result<String> {
    let ch = oxibrain_core::content_hash(text);
    let source = SourceRef::Derived { of: sources.to_vec() };
    let ep_id = oxibrain_core::episode_id(space, &ch, &source, now);
    // INSERT episode (kind='derived')
    // INSERT episode_links (from=derived, to=source, rel='summarizes') for each source
    // INSERT into FTS5 (target_kind='episode', target_id=ep_id, body=text)
    Ok(ep_id)
}
```

- [ ] **Step 5: Implement load_community_entities**

```sql
SELECT label, id FROM communities WHERE space_id=? ORDER BY label, id
```
Group by label into `CommunityGroup`s.

- [ ] **Step 6: Write tests**

- find_episode_clusters finds episodes sharing entities.
- Summary cache round-trip.
- write_derived_episode creates episode + links.
- load_community_entities groups correctly.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(m3): consolidation store primitives"
```

---

### Task 15: Brain consolidation + community summary facades

**Files:**
- Modify: `crates/oxibrain/src/lib.rs`

**Interfaces:**
- Produces: `Brain::consolidate`, `Brain::summarize_communities`

- [ ] **Step 1: Implement Brain::consolidate**

Per spec §11.1 orchestration:
1. Read clusters [reader].
2. For each cluster: check cache [reader]. If miss, build prompt [reader], call LLM [async].
3. Write Derived episodes + cache [WriteOp].

- [ ] **Step 2: Implement Brain::summarize_communities**

Per spec §11.2 — community summaries are Derived episodes with cached text
(DESIGN §5.3, §9.4), same pattern as consolidation:
1. Read community groups [reader].
2. For each group: check cache (`get_cached_summary`) [reader]. If miss, build prompt [reader], call LLM [async].
3. Write Derived episodes + cache text [WriteOp]:
   - `write_derived_episode(conn, space, text, source_episodes, config, now)` — creates a Derived episode + episode_links to episodes mentioning the community's entities.
   - `cache_summary(conn, "community", &member_hash, &extractor_id, &text, now)` — caches for reprojection determinism.
   Both operations run in one WriteOp transaction.

- [ ] **Step 3: Write integration test**

Using FakeLlmPort: declare a graph with two clusters, consolidate, verify Derived episodes created with correct links and cached text.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): consolidation + community summary facades"
```

---

## Sub-Session M3d: Eval Harness + Budget Measurement + CI Gates

### Task 16: Eval metrics + golden corpus

**Files:**
- Create: `eval/Cargo.toml`
- Create: `eval/src/lib.rs`
- Create: `eval/src/metrics.rs`
- Create: `eval/corpus/en/note-001.json` (and more fixtures)
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Produces: `EvalMetrics`, `EvalRunner`, golden corpus fixtures

- [ ] **Step 1: Add eval workspace member**

- [ ] **Step 2: Implement EvalMetrics**

Per spec §14.2:
```rust
pub struct EvalMetrics {
    pub fabricated_entity_rate: f64,
    pub statement_precision: f64,
    pub statement_recall: f64,
    pub resolution_f1: f64,
    pub wrong_merge_rate: f64,
}
```

`from_extraction(actual, expected)` — compares extracted facts against golden annotations. Precision = correct / extracted. Recall = correct / expected. Fabricated-entity rate = entities in extraction not in expected / not in content.

- [ ] **Step 3: Create golden corpus fixtures**

~10 English + ~5 Korean fixtures. Each with: content, expected_entities, expected_claims. Small but diverse (notes, agent traces, temporal).

- [ ] **Step 4: Write tests**

- Metrics compute correctly on a known example.
- Corpus loads.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(m3): eval metrics + golden corpus fixtures"
```

---

### Task 17: Fast eval suite

**Files:**
- Create: `eval/src/fast.rs`
- Modify: `crates/oxibrain-cli/src/cmd/eval.rs`

**Interfaces:**
- Produces: `run_fast_suite()` — replays fixture responses via FakeLlmPort

- [ ] **Step 1: Implement fast suite runner**

1. Load golden corpus.
2. For each fixture: create a FakeLlmPort with a canned ExtractionResponse matching expected_claims.
3. Ingest + extract.
4. Compare extracted assertions against expected.
5. Compute EvalMetrics.
6. Assert §14.2 gates (fabricated_entity_rate == 0.0, precision >= 0.90, etc.).

- [ ] **Step 2: Add CLI subcommand**

`oxibrain eval --suite fast` runs the fast suite. Outputs metrics + pass/fail.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(m3): fast eval suite (fixture-replayed, no network)"
```

---

### Task 18: Budget measurement

**Files:**
- Run existing bench suite, record numbers
- Modify: `doc/DESIGN.md` §13.2 (record measurements)

- [ ] **Step 1: Run bench suite**

```bash
cargo bench -p oxibrain
```

- [ ] **Step 2: Record numbers**

Record p95 for each budget operation in DESIGN.md §13.2. Revise budgets once if needed (D16).

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "docs(m3): record §13.2 budget measurements"
```

---

### Task 19: CLI subcommands + full CI gate

**Files:**
- Modify: `crates/oxibrain-cli/src/cmd/mod.rs`
- Create: `crates/oxibrain-cli/src/cmd/extract.rs`
- Create: `crates/oxibrain-cli/src/cmd/reextract.rs`

- [ ] **Step 1: Add extract/reextract CLI subcommands**

`oxibrain extract [--space S] [--extractor ID]` — runs extract_pending.
`oxibrain reextract [--space S] --extractor ID` — re-extracts with new config.

- [ ] **Step 2: Run full CI gate suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1
cargo deny check
```

- [ ] **Step 3: Fix any failures**

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(m3): CLI extract/reextract subcommands + CI gates"
```

---

### Task 20: Extended regression tests

**Files:**
- Modify: `crates/oxibrain/tests/reproject_determinism.rs`
- Create: `crates/oxibrain/tests/m3_extraction.rs`

- [ ] **Step 1: Extended reprojection determinism test**

Include extraction replay in the byte-identical comparison. Uses FakeLlmPort for deterministic extraction.

- [ ] **Step 2: M3 extraction integration tests**

- End-to-end: ingest → extract → query → verify results.
- Fabricated entity rejection: FakeLlmPort returns a claim with a surface not in the content → validator rejects → quarantine.
- Re-extraction comparability: extract with two extractors, both assertions coexist.
- Repair loop: first response invalid, repair response valid → claims projected.

- [ ] **Step 3: Run all tests**

```bash
cargo test --workspace
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "test(m3): extraction integration + extended reprojection tests"
```

---

## Dependency Graph

```
Task 1 (core types) ──► Task 3 (schema/prompt) ──► Task 10 (validator) ──► Task 11 (pipeline)
                  ──► Task 2 (confidence)                                                       │
                                                                                                ▼
Task 4 (FakeLlm) ──► Task 5 (llm-http) ──► Task 8 (Brain facade) ◄── Task 6 (job queue) ──► Task 9 (project_extraction)
                                                                                                  │
Task 7 (cache CRUD) ──────────────────────────────────────────────────────────────────────────►│
                                                                                                  ▼
                                                                                        Task 12 (reextract+reproject)
                                                                                                  │
Task 13 (quarantine) ──► Task 14 (consolidation store) ──► Task 15 (Brain consolidate)           │
                                                                                                  ▼
                                                                                        Task 16 (eval metrics)
                                                                                                  │
                                                                                        Task 17 (fast suite)
                                                                                                  │
                                                                                        Task 18 (budgets)
                                                                                                  │
                                                                                        Task 19 (CLI + CI)
                                                                                                  │
                                                                                        Task 20 (regression tests)
```

## Self-Review Notes

- **Spec coverage:** All spec sections (§3.1 capabilities) map to tasks. §6 schema → Task 3. §7 validator → Task 10. §8 job queue → Task 6. §9 extraction pipeline → Tasks 9, 11. §10 reproject → Task 12. §11 consolidation → Tasks 14, 15. §12 LLM port → Tasks 4, 5. §13 quarantine → Task 13. §14 eval → Tasks 16, 17. §15 budgets → Task 18. §16 schema → none needed. §17 facade → Task 8.
- **Type consistency:** `ExtractorConfig::id()` used consistently. `project_extraction` signature matches across tasks. `validate_claims` returns `ValidationResult` consumed by `project_extraction` and the repair loop.
- **Compile blockers to trace:** BrainError variants (Task 5 adds Provider/Extraction/Budget — needed by Tasks 8, 11). `resolve_or_create` visibility (Task 6 makes pub(crate) — needed by Task 9). `SourceRef::Derived { of }` — verify this variant exists in core. `Polarity` re-export — verify knowledge::Polarity has Affirm/Deny.

---

End of plan. Read the spec
(`docs/superpowers/specs/2026-08-11-oxibrain-m3-extraction-eval-design.md`) for
full type definitions and architecture rationale.
