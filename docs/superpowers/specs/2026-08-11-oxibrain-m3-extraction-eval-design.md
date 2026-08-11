# oxibrain M3 — Extraction & Evaluation Design Spec

> **Date:** 2026-08-11
> **Authority:** `doc/DESIGN.md` v1.0 (§§5.3, 7, 8, 12.3, 13.2–13.3, 14, 17). This spec
> scopes and concretizes M3. Where this spec and DESIGN.md disagree, DESIGN.md wins
> unless this spec explicitly records a deviation (§18).
> **Predecessor:** M2 Retrieval & Lifecycle (complete — see
> `docs/superpowers/specs/2026-08-11-oxibrain-m2-retrieval-lifecycle-design.md`).
> **Status:** Design. Drives the M3 implementation plan.

---

## 1. Goal

The extraction and evaluation layer: the LLM arrives, but non-determinism is
**contained, cached, and replayable**. M3 delivers the full ingestion pipeline —
job queue, LLM port + HTTP adapter, registry-generated JSON Schema, forced
structured output, validator with repair and quarantine, verbatim mention
capture, extractor identity, re-extraction, budgets and backpressure,
consolidation and community summaries (cached `Derived` episodes, §5.3) — plus
the golden corpus, benchmark runners, and CI quality gates.

The exit condition is a brain that ingests a real note corpus, extracts a valid
knowledge graph, and answers questions with measured quality (§14.2 gates).

## 2. M3 Exit Criteria (DESIGN §17)

1. A real note corpus ingests and produces a graph meeting §14.2 gates
   (fabricated-entity rate = 0.00 structural; statement precision ≥ 0.90;
   recall ≥ 0.70; resolution F1 ≥ 0.92).
2. Benchmark numbers published for reference and default configurations.
3. `reextract` with a second model is comparable on the eval suite.
4. §13.2 budget numbers measured and either met or revised once (deferred from M2).
5. M1 + M2 exit criteria still hold: fold property tests, reprojection
   byte-identical, retrieval and lifecycle still functional.

---

## 3. Scope

### 3.1 In M3

| Capability | Detail |
|---|---|
| Job queue lifecycle | `ingest_jobs` state machine: ready → leased → done / failed; claim/lease/complete/retry/quarantine |
| LLM port + HTTP adapter | `oxibrain-llm-http` crate: Anthropic (structured output via tool use), OpenAI (JSON mode); both behind `LlmPort` |
| Fake LLM port | `FakeLlmPort` in ports: canned responses keyed by content hash — deterministic tests, no network |
| Extractor identity | `ExtractorId = blake3(model_id, prompt_version, registry_major_version, mechanism)`; `ExtractorConfig` |
| Registry-generated schema | `schema_from_registry(predicates) -> JSON Value` — pure fn; constrains predicates, structure, spans |
| Extraction prompt | System prompt generated from registry (descriptions, examples, object types); no hard-coded predicates (P4) |
| Forced structured output | JSON Schema passed via `LlmRequest.json_schema`; mechanism recorded in `ExtractorId` |
| Extraction pipeline | Episode → schema → LLM call (off-actor) → cache → parse → validate → project assertions |
| Validator | Registry-driven: unknown predicate, type mismatch, cardinality, malformed literal, span exists, verbatim mention (§7.4) |
| Repair loop | One retry with validator errors appended; then partial acceptance — valid kept, invalid quarantined |
| Quarantine | `extraction_failures` writes: raw response + errors; browsable, re-runnable |
| Mention capture | Verbatim surface + byte span per subject/object; resolution method recorded (reuses M1 resolution) |
| Re-extraction | `reextract(space, new_extractor_id)` — re-runs extraction with new config; old cache preserved for comparability |
| Consolidation | Cluster related episodes → LLM summarize → `Derived` episode linked to sources (§10); text cached in `summaries` |
| Community summaries | LLM-generate summary text per community; cached in `summaries` (§5.3); terminal — never re-extracted |
| Confidence calibration | `calibrate(extractor_id, eval_data) -> f32`; per-extractor multiplier from eval results (§6.5) |
| Golden corpus | ~50 labeled episodes (note, document, agent-trace shapes; EN + KO) with annotated entities/statements/intervals; ~20 questions with reference answers |
| Eval harness | `fast` suite (fixture-replayed responses, no network, every PR) + `full` suite (live provider, manual/nightly) |
| Budget measurement | Run M2 bench suite, record §13.2 numbers, revise once if needed (deferred from M2) |

### 3.2 Deferred to M4+

| Deferred | Milestone | Why |
|---|---|---|
| Dense GGUF embeddings (`oxibrain-embed-local`) | M4 | DESIGN §17 M3 scope does not list dense embeddings; GGUF runtime is a heavy native dependency. TF-IDF remains the default. See §18 D2. |
| sqlite-vec persistence | M4 | Arrives with dense embeddings needing native vector SQL. TF-IDF vectors use BLOB storage (M2). |
| HNSW approximate kNN | M4 | Brute-force cosine kNN is deterministic and adequate. HNSW needs deterministic level assignment. |
| MCP sampling as LlmPort | M4 | Session-bound; requires MCP server + `Sample` capability (§12.3). M3 uses HTTP adapters only. |
| Spaces/scopes/tokens/audit/trust/redaction | M4 | Security/tenancy milestone |
| MCP server, full CLI, connectors | M4 | Surfaces milestone |

### 3.3 What "the LLM arrives" means for determinism

M3 introduces the first non-deterministic element: the LLM call. The design
contains it through three mechanisms:

1. **Response cache** (`extractions` table): every LLM response is cached,
   keyed by `(episode_id, extractor_id)`. Reprojection reads the cache and
   never calls the LLM. Same cache → same assertions → byte-identical projection.
2. **Off-actor execution**: LLM calls run in `spawn_blocking` tasks, never
   inside a database transaction (§7.2). The writer actor only sees short
   `WriteOp`s that write cached results.
3. **Validator gate**: all LLM output passes through `validate_claims` before
   reaching `beliefs`. Invalid output goes to `extraction_failures`, never to
   the projection (§7.4, P2).

The reprojection determinism test (§14.3) extends to include extraction replay:
the projection built incrementally (ingest → extract → project) must be
byte-identical to the projection built by reproject (replay declarations +
replay extractions from cache). No LLM call on reproject.

---

## 4. Architecture

### 4.1 Dependency DAG

M2 established: `ports ← core ← index ← store ← oxibrain` (facade). M3 adds
`oxibrain-llm-http` as an adapter crate and extends `oxibrain-ports` with a
`FakeLlmPort`:

```
ports          ← base types: Timestamp, BrainError, ClockPort, LlmPort,
                  EmbeddingPort + FakeLlmPort (test)
  ↑
core           ← types + pure logic: Claim, MentionRef, ExtractionResponse,
                  schema_from_registry, validate_claims, calibrate
  ↑
index          ← unchanged (M2 algorithms)
  ↑
store          ← persistence + execution: job queue CRUD, cache CRUD,
                  project_extraction, quarantine, consolidation, reproject ext.
  ↑
oxibrain       ← facade: Brain gains Option<Arc<dyn LlmPort>>;
                  ingest, extract, reextract, consolidate, eval
  ↑
oxibrain-llm-http  ← NEW adapter: Anthropic + OpenAI behind LlmPort.
                      Depends on ports + reqwest. Feature-gated.
```

**Dependency rules (DESIGN §15, enforced):**
- `oxibrain-llm-http` depends on `oxibrain-ports` only. Never on core, store, or index.
- `oxibrain-core` stays pure — no LLM, no network, no I/O. Schema generation and
  validation are pure functions of the registry.
- `oxibrain-store` orchestrates: calls LLM (via the port, off-actor), writes
  assertions (inside transactions).
- Default features pull **zero** oxi-ecosystem crates and zero HTTP dependencies.
  `cargo build -p oxibrain --no-default-features --features http-llm` produces a
  working standalone brain.

### 4.2 Extraction execution flow

```
Brain::ingest(space, content)
  │
  ├─ WriteOp [actor, tx]:
  │    create episode (Primary) + insert ingest_job(state=ready)
  │
  └─ return episode_id

Brain::extract_pending(space, extractor_config)   ← batch worker
  │
  ├─ 1. Claim jobs [actor, tx]:
  │     UPDATE ingest_jobs SET state='leased', lease_until=now+timeout
  │     WHERE state='ready' AND extractor_id=? LIMIT N
  │     SELECT * FROM ingest_jobs WHERE state='leased' AND ...
  │
  ├─ 2. For each claimed job [OFF-actor]:
  │     a. Read episode content [reader pool]
  │     b. Generate schema [pure fn: registry → JSON Schema]
  │     c. Build prompt [pure fn: registry → system prompt + episode text]
  │     d. Call LLM [spawn_blocking, async: llm.complete(req)]
  │     e. Parse response [pure fn: raw JSON → Vec<Claim>]
  │     f. Validate claims [pure fn: claims + content + registry → ValidationResult]
  │     g. If invalid claims exist AND attempts < max_repair:
  │        - Rebuild prompt with validator errors appended
  │        - Call LLM again [spawn_blocking]
  │        - Re-parse, re-validate
  │
  ├─ 3. Project [actor, tx — WriteOp]:
  │     a. Cache raw response: INSERT INTO extractions(...)
  │     b. For each valid claim:
  │        - Resolve entities (reuse M1 resolve_or_create with real byte spans)
  │        - Create statement (idempotent)
  │        - Create assertion (extractor_id = config.id(), confidence from LLM)
  │        - Capture mentions (verbatim surface + byte span)
  │     c. Re-fold affected belief groups
  │     d. Update FTS5/TF-IDF indexes for new entities/statements
  │     e. For each invalid claim: INSERT INTO extraction_failures(...)
  │     f. UPDATE ingest_job SET state='done' (or 'failed' if all claims invalid)
  │
  └─ return ExtractSummary { extracted, quarantined, failed }
```

**Critical constraint (§7.2):** steps 2a–2g run **outside** any transaction.
Step 3 is a single `WriteOp` closure that runs inside one transaction on the
writer actor. The LLM is never called inside a transaction. A stalled provider
never blocks readers or the writer.

### 4.3 Module map

```
oxibrain-core/src/
  extraction.rs         # NEW — Claim, ClaimObject, MentionRef, ExtractionResponse,
                        #         schema_from_registry (pure fn),
                        #         build_extraction_prompt (pure fn),
                        #         validate_claims (pure fn), ValidationError,
                        #         ValidationResult, ExtractorConfig, ExtractorId
  confidence.rs         # NEW — calibrate(extractor, eval_data) -> f32,
                        #         ConfidenceComponents, combine_confidence

oxibrain-ports/src/
  llm.rs                # EXTEND — LlmRequest/LlmResponse already sufficient;
                        #   add FakeLlmPort impl (test-only, behind feature)
  llm_fake.rs           # NEW — FakeLlmPort: HashMap<content_hash, response>

oxibrain-llm-http/      # NEW crate
  Cargo.toml
  src/
    lib.rs              # re-exports
    anthropic.rs        # Anthropic adapter (structured output via tool use)
    openai.rs           # OpenAI adapter (JSON mode / structured output)
    error.rs            # HttpLlmError → BrainError mapping

oxibrain-store/src/
  extraction.rs         # NEW — job queue CRUD (claim/lease/complete/retry/fail),
                        #         cache CRUD (get/put raw response),
                        #         project_extraction (claims → assertions),
                        #         extract_offline (parse+validate+project from cache)
  quarantine.rs         # NEW — record_failure, list_failures, retry_failure
  consolidation.rs      # NEW — consolidate: cluster episodes → summarize → Derived ep
                        #         summarize_community: community → summary text (cached)

oxibrain/src/
  lib.rs                # EXTEND — Brain gains Option<Arc<dyn LlmPort>>;
                        #   ingest (creates job), extract_pending, extract_one,
                        #   reextract, consolidate, summarize_communities,
                        #   job_status, extraction_failures

oxibrain-cli/src/
  cmd/
    extract.rs          # NEW — `oxibrain extract` subcommand
    eval.rs             # NEW — `oxibrain eval` subcommand
    reextract.rs        # NEW — `oxibrain reextract` subcommand

eval/                   # NEW — golden corpus + benchmark runners
  Cargo.toml
  src/
    lib.rs              # EvalRunner, EvalConfig
    corpus.rs           # Golden corpus loader
    metrics.rs          # precision, recall, F1, fabricated-entity rate
    fast.rs             # fast suite (fixture-replayed)
  corpus/
    en/                 # English labeled episodes + questions
    ko/                 # Korean labeled episodes + questions
```

### 4.4 New workspace dependencies

```toml
# Added to [workspace.dependencies]:
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
```

`oxibrain-llm-http` crate manifest:

```toml
[package]
name = "oxibrain-llm-http"
edition.workspace = true

[dependencies]
oxibrain-ports.workspace = true
async-trait.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

The facade (`oxibrain`) gains `oxibrain-llm-http` as an optional dependency:

```toml
[features]
http-llm = ["dep:oxibrain-llm-http"]
```

`eval/` is a workspace member with `oxibrain` + `oxibrain-store` + `proptest` dev-deps.

---

## 5. Data types

### 5.1 Extraction types (core/extraction.rs)

```rust
use crate::knowledge::{EntityId, EntityTypeRef, PredicateRef};
use crate::registry::{PredicateDef, ObjectKind, LiteralType};
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

/// Configuration for an extractor: model, prompt version, mechanism.
/// Hashes to an ExtractorId (§7.5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractorConfig {
    pub model_id: String,           // e.g. "claude-sonnet-4-5"
    pub prompt_version: u32,        // bump when the prompt template changes
    pub registry_major: u32,        // CORE_V1_MAJOR (from registry)
    pub mechanism: ExtractMechanism, // how structured output is enforced
    pub max_tokens: u32,            // per-episode token budget
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractMechanism {
    /// Provider-native JSON Schema structured output (OpenAI, Anthropic tool use).
    JsonSchema,
    /// Forced tool call (Anthropic fallback when JSON Schema mode unavailable).
    ToolCall,
    /// JSON mode without schema enforcement (weakest; validator is the only gate).
    JsonMode,
}

impl ExtractorConfig {
    /// ExtractorId = blake3(model_id, prompt_version, registry_major, mechanism).
    /// Only the MAJOR registry version invalidates the cache (D8).
    pub fn id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.model_id.as_bytes());
        hasher.update(&self.prompt_version.to_le_bytes());
        hasher.update(&self.registry_major.to_le_bytes());
        hasher.update([self.mechanism as u8]);
        hex::encode(hasher.finalize().as_bytes())
    }
}

/// A reference to an entity mention in the episode text.
/// Surface must appear verbatim at the given byte span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MentionRef {
    pub surface: String,        // verbatim text from the episode
    pub entity_type: String,    // Person, Organization, Project, ...
    pub span: (u32, u32),       // byte offsets into episode content
}

/// The object of an extracted claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimObject {
    Entity {
        mention: MentionRef,
    },
    Literal {
        literal_type: String,   // text, date, datetime, number, bool
        value: String,
        span: (u32, u32),       // byte offsets of the value in the episode
    },
}

/// One extracted claim from the LLM. Maps to one assertion + mentions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub predicate: String,      // must exist in the registry
    pub subject: MentionRef,
    pub object: ClaimObject,
    pub polarity: Polarity,     // Affirm | Deny
    pub valid_from: Option<i64>,// epoch millis; None = TIME_MIN
    pub valid_to: Option<i64>,  // epoch millis; None = TIME_MAX
    pub confidence: f32,        // [0.0, 1.0] from the LLM
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    Affirm,
    Deny,
}

/// The parsed LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResponse {
    pub claims: Vec<Claim>,
}

/// Result of validating claims against the registry + episode content.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub valid: Vec<Claim>,
    pub invalid: Vec<(Claim, Vec<ValidationError>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidationError {
    UnknownPredicate { predicate: String },
    SubjectTypeMismatch { predicate: String, expected: Vec<String>, got: String },
    ObjectTypeMismatch { predicate: String, expected: String, got: String },
    MalformedLiteral { literal_type: String, value: String, reason: String },
    SpanOutOfBounds { span: (u32, u32), content_len: usize },
    SurfaceNotVerbatim { surface: String, span: (u32, u32), found: String },
    ConfidenceOutOfRange { confidence: f32 },
}

/// Summary of an extraction batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractSummary {
    pub extracted: usize,       // valid claims projected
    pub quarantined: usize,     // invalid claims filed
    pub episodes_done: usize,   // jobs completed
    pub episodes_failed: usize, // jobs that exhausted retries
}
```

### 5.2 Confidence types (core/confidence.rs)

```rust
use serde::{Deserialize, Serialize};

/// Components of the confidence formula (DESIGN §6.5):
///   confidence = calibrate(extractor) · corroboration · trust · recency
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceComponents {
    pub raw: f32,             // from the LLM assertion
    pub calibrated: f32,      // calibrate(extractor) multiplier [0.1, 2.0]
    pub corroboration: f32,   // saturating in distinct supporting episodes [0.5, 1.0]
    pub trust: f32,           // weighted by episode trust tier [0.3, 1.0]
    pub recency: f32,         // recency_of_support for Interval predicates [0.5, 1.0]
}

impl ConfidenceComponents {
    /// Compute final confidence. Clamped to [0.0, 1.0].
    pub fn combine(&self) -> f32 {
        let c = self.raw * self.calibrated * self.corroboration * self.trust * self.recency;
        c.clamp(0.0, 1.0)
    }
}

/// Per-extractor calibration multiplier, measured by the eval harness.
/// An unmeasured extractor gets a conservative prior of 0.8.
pub fn calibrate(extractor_id: &str, calibration_table: &CalibrationTable) -> f32 {
    calibration_table.get(extractor_id).unwrap_or(0.8)
}

/// Stores per-extractor calibration values (loaded from eval results).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CalibrationTable {
    pub values: std::collections::BTreeMap<String, f32>,
}

impl CalibrationTable {
    pub fn get(&self, extractor_id: &str) -> Option<f32> {
        self.values.get(extractor_id).copied()
    }
    pub fn set(&mut self, extractor_id: &str, value: f32) {
        self.values.insert(extractor_id.to_string(), value.clamp(0.1, 2.0));
    }
}
```

### 5.3 Job queue types (store/extraction.rs)

```rust
use oxibrain_ports::Timestamp;

/// A row from the ingest_jobs table.
#[derive(Debug, Clone)]
pub struct IngestJob {
    pub id: String,
    pub episode_id: String,
    pub extractor_id: String,
    pub state: JobState,
    pub session_hint: Option<String>,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub lease_until: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Ready,    // waiting to be claimed
    Leased,   // a worker is processing it
    Done,     // extraction succeeded
    Failed,   // exhausted retries → quarantine
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Leased => "leased",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ready" => Some(Self::Ready),
            "leased" => Some(Self::Leased),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}
```

---

## 6. Schema generation

### 6.1 `schema_from_registry` — pure fn

Generates a JSON Schema from the predicate registry. The schema constrains the
LLM's output structure: valid predicate names, mention shape (surface + type +
span), object shape (entity or literal), polarity, confidence range.

```rust
/// Generate the extraction JSON Schema from the registry (P4).
/// Pure function: same registry → same schema.
pub fn schema_from_registry(predicates: &[PredicateDef]) -> serde_json::Value {
    let pred_names: Vec<&str> = predicates.iter()
        .map(|p| p.name.as_str()).collect();
    let entity_types: Vec<&str> = predicates.iter()
        .flat_map(|p| p.subject_types.iter().map(|t| t.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter().collect();

    serde_json::json!({
        "type": "object",
        "properties": {
            "claims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "predicate": {
                            "type": "string",
                            "enum": pred_names
                        },
                        "subject": mention_schema(&entity_types),
                        "object": object_schema(),
                        "polarity": {
                            "type": "string",
                            "enum": ["affirm", "deny"]
                        },
                        "valid_from": {
                            "type": ["integer", "null"],
                            "description": "epoch millis, or null for 'always'"
                        },
                        "valid_to": {
                            "type": ["integer", "null"],
                            "description": "epoch millis, or null for 'still true'"
                        },
                        "confidence": {
                            "type": "number",
                            "minimum": 0.0,
                            "maximum": 1.0
                        }
                    },
                    "required": ["predicate", "subject", "object", "polarity", "confidence"]
                }
            }
        },
        "required": ["claims"]
    })
}
```

**Why the schema doesn't enforce predicate→object-type mapping via JSON Schema
conditionals:** JSON Schema `if/then/then` branches are brittle, model-dependent,
and produce very large schemas for ~40 predicates. Instead, the schema constrains
**structure** (valid predicate names, mention shape, confidence range), and the
**validator** (§7) enforces **semantics** (predicate↔object-type matching,
cardinality, span validity, verbatim surface). Both are generated from the same
registry — P4 is satisfied. This split is documented as a design choice, not a
limitation: the schema does what JSON Schema does well (structural shape); the
validator does what Rust does well (semantic rules with typed errors).

### 6.2 Prompt generation — pure fn

```rust
/// Build the extraction system prompt from the registry (P4: no hard-coded predicates).
pub fn build_extraction_prompt(predicates: &[PredicateDef]) -> String {
    let mut s = String::new();
    s.push_str("You are a knowledge extraction engine. Extract structured claims ");
    s.push_str("from the given text. Each claim references entities by their ");
    s.push_str("VERBATIM surface form and byte span in the text.\n\n");
    s.push_str("Available predicates:\n");
    for p in predicates {
        s.push_str(&format!(
            "- {} ({}): {}\n",
            p.name,
            match &p.object_kind {
                ObjectKind::Entity(t) => format!("entity: {t}"),
                ObjectKind::Literal(lt) => format!("literal: {:?}", lt),
            },
            p.description
        ));
        if !p.examples.is_empty() {
            s.push_str(&format!("  Examples: {}\n", p.examples.join("; ")));
        }
    }
    s.push_str("\nReturn JSON matching the provided schema. For each entity mention, ");
    s.push_str("provide the surface text exactly as it appears, its type, and the ");
    s.push_str("byte offset range [start, end) where it appears in the text.");
    s
}
```

---

## 7. Validator

### 7.1 `validate_claims` — pure fn

Validates parsed claims against the registry and the episode content. Returns
valid claims (accepted) and invalid claims with their errors (quarantined).

```rust
/// Validate claims against the registry + episode content (§7.4).
/// Pure function: same claims + content + registry → same result.
pub fn validate_claims(
    claims: &[Claim],
    content: &str,
    predicates: &[PredicateDef],
) -> ValidationResult {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();

    for claim in claims {
        let mut errors = Vec::new();

        // 1. Confidence range.
        if !(0.0..=1.0).contains(&claim.confidence) {
            errors.push(ValidationError::ConfidenceOutOfRange {
                confidence: claim.confidence,
            });
        }

        // 2. Predicate exists in registry.
        let pred_def = predicates.iter().find(|p| p.name == claim.predicate);
        let pred_def = match pred_def {
            Some(p) => p,
            None => {
                errors.push(ValidationError::UnknownPredicate {
                    predicate: claim.predicate.clone(),
                });
                invalid.push((claim.clone(), errors));
                continue;
            }
        };

        // 3. Subject type matches predicate.subject_types.
        if !pred_def.subject_types.iter().any(|t| t == &claim.subject.entity_type) {
            errors.push(ValidationError::SubjectTypeMismatch {
                predicate: claim.predicate.clone(),
                expected: pred_def.subject_types.iter().map(|t| t.to_string()).collect(),
                got: claim.subject.entity_type.clone(),
            });
        }

        // 4. Object type matches predicate.object_kind.
        match (&claim.object, &pred_def.object_kind) {
            (ClaimObject::Entity { mention }, ObjectKind::Entity(expected_ty)) => {
                if &mention.entity_type != expected_ty {
                    errors.push(ValidationError::ObjectTypeMismatch {
                        predicate: claim.predicate.clone(),
                        expected: expected_ty.clone(),
                        got: mention.entity_type.clone(),
                    });
                }
            }
            (ClaimObject::Literal { literal_type, .. }, ObjectKind::Literal(expected_lt)) => {
                if !literal_type_matches(literal_type, expected_lt) {
                    errors.push(ValidationError::ObjectTypeMismatch {
                        predicate: claim.predicate.clone(),
                        expected: format!("{:?}", expected_lt),
                        got: literal_type.clone(),
                    });
                }
            }
            (ClaimObject::Entity { .. }, ObjectKind::Literal(_))
            | (ClaimObject::Literal { .. }, ObjectKind::Entity(_)) => {
                errors.push(ValidationError::ObjectTypeMismatch {
                    predicate: claim.predicate.clone(),
                    expected: format!("{:?}", pred_def.object_kind),
                    got: match &claim.object {
                        ClaimObject::Entity { .. } => "entity".into(),
                        ClaimObject::Literal { .. } => "literal".into(),
                    },
                });
            }
        }

        // 5. Subject span exists in content.
        check_span(&claim.subject.span, content, &claim.subject.surface, &mut errors);

        // 6. Object span (for entities and literals with spans).
        match &claim.object {
            ClaimObject::Entity { mention } => {
                check_span(&mention.span, content, &mention.surface, &mut errors);
            }
            ClaimObject::Literal { span, .. } => {
                check_span(span, content, "", &mut errors);
            }
        }

        // 7. Surface forms are verbatim at the given spans (fabricated-entity gate).
        check_verbatim(&claim.subject, content, &mut errors);
        if let ClaimObject::Entity { mention } = &claim.object {
            check_verbatim(mention, content, &mut errors);
        }

        if errors.is_empty() {
            valid.push(claim.clone());
        } else {
            invalid.push((claim.clone(), errors));
        }
    }

    ValidationResult { valid, invalid }
}
```

### 7.2 The verbatim-mention rule (§7.4)

The validator checks that each entity mention's `surface` appears **exactly** at
the byte range `[span.0, span.1)` in the episode content:

```rust
fn check_verbatim(m: &MentionRef, content: &str, errors: &mut Vec<ValidationError>) {
    let bytes = content.as_bytes();
    if m.span.0 as usize >= bytes.len() || m.span.1 as usize > bytes.len() {
        // SpanOutOfBounds already recorded by check_span.
        return;
    }
    let found = &content[m.span.0 as usize..m.span.1 as usize];
    if found != m.surface {
        errors.push(ValidationError::SurfaceNotVerbatim {
            surface: m.surface.clone(),
            span: m.span,
            found: found.to_string(),
        });
    }
}
```

This structurally prevents fabricated entities (§7.4): a name not in the text
cannot pass. It does **not** prevent false relationships between two real
entities — that is defended by eval, not architecture.

### 7.3 Repair loop (§7.4)

After the first extraction + validation:

1. If `invalid` is non-empty AND `job.attempts < max_repair` (default 1):
   - Append validator errors to the prompt: "The following claims were invalid:
     {errors}. Please fix them and re-extract."
   - Call the LLM again.
   - Re-parse, re-validate.
2. After the repair attempt (or if max_repair reached): **partial acceptance** —
   valid claims are projected; invalid claims are filed in `extraction_failures`.
3. A bad batch never blocks a good one and never disappears silently.

---

## 8. Job queue lifecycle

### 8.1 State machine

```
                ┌─────────┐
   ingest ────► │  ready  │ ◄──── retry (attempts < max)
                └────┬────┘
                     │ claim (lease_until = now + timeout)
                     ▼
                ┌─────────┐
                │ leased  │
                └────┬────┘
              success │ │ failure (attempts >= max)
            ┌────────┘ └────────┐
            ▼                     ▼
       ┌─────────┐          ┌─────────┐
       │  done   │          │ failed  │ ──► extraction_failures
       └─────────┘          └─────────┘
```

**Lease expiry:** a background sweep (or next `claim_jobs` call) reclaims jobs
where `state='leased' AND lease_until < now`, setting them back to `ready`. This
handles crashed workers.

### 8.2 Store functions

```rust
// store/extraction.rs

/// Enqueue an extraction job for an episode.
pub fn enqueue_job(conn, episode_id, extractor_id, now) -> Result<String>;

/// Claim up to `limit` ready jobs for an extractor. Sets state=leased.
pub fn claim_jobs(conn, extractor_id, lease_timeout_secs, limit, now) -> Result<Vec<IngestJob>>;

/// Complete a job: state=done.
pub fn complete_job(conn, job_id, now) -> Result<()>;

/// Fail a job: increment attempts. If attempts >= max, state=failed; else state=ready.
pub fn fail_job(conn, job_id, error, max_attempts, now) -> Result<JobState>;

/// Reclaim expired leases: state=leased AND lease_until < now → state=ready.
pub fn reclaim_expired(conn, now) -> Result<usize>;

/// List jobs by state (for status queries).
pub fn list_jobs(conn, state: Option<JobState>) -> Result<Vec<IngestJob>>;
```

### 8.3 Budget + backpressure (§7.6)

```rust
/// Extraction budget limits. The queue holds on exhaustion; it never drops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionBudget {
    pub max_concurrent: usize,    // max parallel LLM calls (default 4)
    pub max_episodes_per_batch: usize, // max jobs claimed per batch (default 50)
    pub max_tokens_per_episode: u32,   // token budget per LLM call (default 8192)
    pub max_repair_attempts: u32,      // repair retries before quarantine (default 1)
    pub lease_timeout_secs: u64,       // lease duration (default 300)
}
```

**Profiles (§7.6):**
- `realtime` — extract immediately (sync mode: `extract_one`)
- `batched` (default) — extract on interval (`extract_pending` with a batch size)
- `nightly` — consolidation window, cheapest tier (future: model selection per profile)

---

## 9. Extraction pipeline

### 9.1 `project_extraction` — the projection from claims

Reuses the M1 `resolve_or_create` pattern. For each valid claim:

1. Resolve the subject entity (normalize → resolve → link/new/candidate).
   Entity ID is derived from `(space, type, episode_id, span_start)` (§5.6).
   This is the **same** derivation as declarations, so entities extracted from
   the same episode at the same span get the same ID regardless of extractor.
2. Resolve the object entity (if `ClaimObject::Entity`).
3. Create the statement (idempotent, content-derived ID).
4. Create the assertion with `extractor_id = config.id()`, confidence from the
   LLM, `claimed_from/_to` from the claim (or sentinels if `None`).
5. Capture mentions with real byte spans (unlike declarations which use fixed
   offsets 0/100/200).
6. Re-fold the affected belief group (same `fold` + `replace_beliefs` as M1).

**Refactor:** `resolve_or_create` in `project.rs` becomes `pub(crate)` so
`extraction.rs` can call it. The function already takes `(conn, space, eref,
episode_id, span_start, now)` — extraction passes the real `span.0` from the
mention, not a fixed offset.

```rust
// store/extraction.rs

/// Project valid claims from an extraction into assertions + mentions.
/// Runs inside a WriteOp transaction. Idempotent (content-derived IDs).
pub fn project_extraction(
    conn: &Connection,
    space: &str,
    episode_id: &str,
    extractor_id: &str,
    claims: &[Claim],
    now: Timestamp,
) -> Result<usize, BrainError> {
    let mut count = 0;
    for claim in claims {
        // Resolve subject entity.
        let subj_eref = EntityRef {
            surface: claim.subject.surface.clone(),
            ty: claim.subject.entity_type.clone(),
        };
        let (subj_id, subj_method) = crate::project::resolve_or_create(
            conn, space, &subj_eref, episode_id, claim.subject.span.0, now,
        )?;

        // Resolve object.
        let (object, obj_mention_data) = resolve_claim_object(conn, space, claim, episode_id, now)?;

        // Create statement (idempotent).
        let stmt_id = statement_id(space, &subj_id, &claim.predicate, &object);
        // ... insert_statement, insert_assertion (with extractor_id + confidence),
        //     insert_mentions (with real spans), re-fold group ...
        count += 1;
    }
    Ok(count)
}
```

### 9.2 Cache CRUD

```rust
/// Cache a raw LLM response for an episode + extractor.
pub fn cache_response(conn, episode_id, extractor_id, raw_response, now) -> Result<()>;

/// Get a cached response (for reproject / re-extraction check).
pub fn get_cached_response(conn, episode_id, extractor_id) -> Result<Option<String>>;
```

The cache is keyed by `PRIMARY KEY (episode_id, extractor_id)` — re-extraction
with the same extractor is a no-op (§7.3 idempotency layer 2).

### 9.3 Re-extraction

```rust
/// Re-extract all episodes in a space with a new extractor config.
/// Old cache entries are preserved (different extractor_id = different PK).
pub async fn reextract(&self, space: &str, config: &ExtractorConfig) -> Result<ExtractSummary>;
```

For each episode:
1. Check cache: `get_cached_response(episode_id, config.id())`. If hit, skip
   (already extracted with this extractor).
2. If miss: create a new `ingest_job` with the new extractor_id, then run the
   extraction pipeline.
3. Both old and new assertions coexist (different extractor_id → comparable on
   the eval suite). Promoting the new extractor is a config change.

### 9.4 Offline extraction (for reprojection)

```rust
/// Parse + validate + project from a cached response — no LLM call.
/// Used by reproject to replay extractions deterministically.
pub fn project_from_cache(
    conn: &Connection,
    space: &str,
    episode_id: &str,
    extractor_id: &str,
    raw_response: &str,
    content: &str,
    predicates: &[PredicateDef],
    now: Timestamp,
) -> Result<usize, BrainError> {
    let response: ExtractionResponse = serde_json::from_str(raw_response)?;
    let result = validate_claims(&response.claims, content, predicates);
    // Project valid claims. Invalid claims are NOT re-filed on reproject —
    // they were already filed during the original extraction.
    project_extraction(conn, space, episode_id, extractor_id, &result.valid, now)
}
```

---

## 10. Reprojection extension (determinism)

### 10.1 Extended reproject flow

`reproject.rs` gains a step between declaration replay and index rebuild:

```
reproject(conn):
  1. Delete projection tables (M1).
  2. Delete index tables (M2).
  3. Replay Declaration episodes (M1).
  4. NEW: Replay Primary episode extractions from cache:
     a. SELECT episode_id, extractor_id, raw_response FROM extractions
        JOIN episodes ON extractions.episode_id = episodes.id
        WHERE episodes.kind = 'primary'
        ORDER BY episodes.seq ASC, extractions.extractor_id ASC
     b. For each cached extraction:
        - Read episode content from episodes table
        - Parse + validate + project (deterministic from cache — no LLM)
        - Pass episode.ingested_at as `now` (same pattern as declarations)
  5. Rebuild indexes + communities (M2).
```

**Canonical replay order (§5.6):** `(episode.seq, extractor_id)`. Within one
episode, claims are ordered by their position in the LLM response (statement
index). This is deterministic because the cached response is byte-identical.

### 10.2 Byte-identical test extension

The `reproject_is_byte_identical` test extends to include:
- Assertions with `extractor_id IS NOT NULL` (extraction-produced)
- Mentions with real byte spans
- Beliefs folded from extraction-produced assertions

The test flow:
1. Build a brain with declarations + primary episodes.
2. Extract primary episodes using a `FakeLlmPort` (deterministic canned responses).
3. Snapshot the projection.
4. `reproject()`.
5. Snapshot again.
6. Assert byte-identical.

---

## 11. Consolidation + community summaries

### 11.1 Architecture: store provides DB primitives, Brain orchestrates LLM

Consolidation and community summarization involve LLM calls. Per §7.2, LLM
calls **never** run inside a store function holding a `&Connection`. Store
provides pure DB primitives; the Brain facade orchestrates: read (reader pool)
→ LLM call (async, off-actor) → write (WriteOp on actor).

**Store primitives (`store/consolidation.rs`):**

```rust
/// Find clusters of episodes sharing ≥ 2 entities within a time window.
pub fn find_episode_clusters(conn: &Connection, space: &str) -> Result<Vec<EpisodeCluster>, BrainError>;

/// Check the summaries cache for a given scope + member set + extractor.
pub fn get_cached_summary(conn: &Connection, scope_kind: &str, member_hash: &[u8], extractor_id: &str)
    -> Result<Option<String>, BrainError>;

/// Cache a summary text.
pub fn cache_summary(conn: &Connection, scope_kind: &str, member_hash: &[u8],
                     extractor_id: &str, text: &str, now: Timestamp) -> Result<(), BrainError>;

/// Write a Derived episode + episode_links to sources. Returns the episode id.
/// The summary text is also indexed by FTS5 (searchable).
pub fn write_derived_episode(conn: &Connection, space: &str, text: &str,
                             sources: &[String], config: &ExtractorConfig, now: Timestamp)
    -> Result<String, BrainError>;

/// Load community entities grouped by label (for summarization).
pub fn load_community_entities(conn: &Connection, space: &str)
    -> Result<Vec<CommunityGroup>, BrainError>;

/// Build the consolidation prompt from an episode cluster (pure DB reads).
pub fn build_consolidation_prompt(conn: &Connection, space: &str, cluster: &EpisodeCluster)
    -> Result<String, BrainError>;

/// Build the community summary prompt from entity beliefs (pure DB reads).
pub fn build_community_prompt(conn: &Connection, space: &str, group: &CommunityGroup)
    -> Result<String, BrainError>;
```

**Brain facade orchestration (`Brain::consolidate`):**

```rust
pub async fn consolidate(&self, space: &str, config: &ExtractorConfig)
    -> Result<Vec<String>, BrainError>
{
    let llm = self.llm.as_ref().ok_or_else(|| BrainError::Config("no LLM port".into()))?;

    // 1. Read clusters [reader pool, spawn_blocking]
    let clusters = self.read_clusters(space).await?;

    // 2. For each cluster: check cache, if miss call LLM [async, off-actor]
    let mut summaries: Vec<(EpisodeCluster, String)> = Vec::new();
    for cluster in clusters {
        let member_hash = hash_member_set(&cluster.episode_ids);
        let cached = self.check_cache("consolidation", &member_hash, &config.id()).await?;
        match cached {
            Some(text) => summaries.push((cluster, text)),   // cache hit
            None => {
                let prompt = self.build_consolidation_prompt(&cluster).await?;
                let response = llm.complete(LlmRequest { .. }).await?;  // off-actor
                summaries.push((cluster, response.text));
            }
        }
    }

    // 3. Write Derived episodes + cache summaries [WriteOp on actor]
    self.write_consolidation(space, &summaries, config).await
}
```

`write_derived_episode` creates:
- An `Episode` with `kind = Derived`, `source = Derived { of: Vec<EpisodeId> }`.
- `episode_links` rows linking the derived episode to its sources (`rel = 'summarizes'`).
- FTS5 index entry for the summary text (searchable).

**Derived episodes are terminal (§5.3):** no assertion is ever extracted from
one. The extraction pipeline only processes `kind = 'primary'` episodes.

### 11.2 Community summaries (§9.4)

M2 built deterministic community clustering. M3 adds LLM-generated summary text,
using the same store-primitives + Brain-orchestration pattern:

1. Brain reads community groups (`load_community_entities`) via reader pool.
2. For each group: check cache (`get_cached_summary`). If miss, call LLM (async).
3. Brain writes cached text via WriteOp (`cache_summary`).

The summary text is cached and indexed by FTS5 (searchable). It is NOT a Derived
episode — it lives in the `summaries` cache table and is surfaced by community
queries (§9.4). Regeneration (`--regenerate-summaries`) creates a new cache entry
with a new extractor_id, leaving the old intact (§5.3).

### 11.3 Why LLM calls cannot be in store functions

Store functions take `&Connection` and run on the writer actor thread (for
writes) or reader pool threads (for reads). The `LlmPort::complete` method is
`async`. A `&Connection` is `!Send` — it cannot cross the `spawn_blocking`
boundary that async requires. Therefore, all LLM call sites live in the Brain
facade, which can freely use `async` and `spawn_blocking` for DB access between
LLM calls. This is the same constraint as the extraction pipeline (§4.2).

---

## 12. LLM port + HTTP adapter

### 12.1 The port (already exists)

The `LlmPort` trait (M0) is already sufficient:

```rust
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    pub json_schema: Option<serde_json::Value>,
    pub max_tokens: u32,
}
pub struct LlmResponse {
    pub text: String,
    pub raw: serde_json::Value,
}
#[async_trait]
pub trait LlmPort: Send + Sync {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError>;
}
```

### 12.2 Anthropic adapter (`oxibrain-llm-http/src/anthropic.rs`)

Uses the Anthropic Messages API with **tool use** for structured output:
- The JSON Schema is wrapped as a tool definition with `input_schema`.
- The model is forced to call the tool (`tool_choice: { type: "tool", name: "extract_claims" }`).
- The tool call arguments are the structured extraction response.
- Mechanism: `ExtractMechanism::ToolCall`.

### 12.3 OpenAI adapter (`oxibrain-llm-http/src/openai.rs`)

Uses the OpenAI Chat Completions API with **structured output**:
- `response_format: { type: "json_schema", json_schema: { schema: ... } }`.
- The model returns JSON matching the schema.
- Mechanism: `ExtractMechanism::JsonSchema`.

### 12.4 FakeLlmPort (`ports/src/llm_fake.rs`)

```rust
/// A test-only LLM port that returns canned responses.
/// Keyed by content hash → response. Deterministic, no network.
pub struct FakeLlmPort {
    responses: std::collections::HashMap<String, LlmResponse>,
    /// Optional: a fallback that generates a response from the prompt
    /// (for fuzzing / property tests).
    fallback: Option<Box<dyn Fn(&LlmRequest) -> LlmResponse + Send + Sync>>,
}
```

The `fast` eval suite uses `FakeLlmPort` with fixture-replayed responses (§14.2).

### 12.5 Brain gains an optional LLM port

```rust
pub struct Brain {
    handle: Arc<StoreHandle>,
    clock: Arc<dyn ClockPort>,
    llm: Option<Arc<dyn LlmPort>>,  // NEW — None = no extraction
}
```

Methods that need the LLM (`extract_pending`, `consolidate`, `reextract`) return
`Err(BrainError::Config("no LLM port configured"))` if `self.llm` is `None`.
Query, traverse, timeline, etc. work without an LLM (P6: the engine is a library).

---

## 13. Quarantine

### 13.1 Store functions

```rust
// store/quarantine.rs

/// Record an extraction failure (invalid claims that exhausted repair).
pub fn record_failure(
    conn, episode_id, extractor_id, raw_response, errors_json, now,
) -> Result<i64>;

/// List all extraction failures (browsable via CLI / MCP).
pub fn list_failures(conn, space: Option<&str>) -> Result<Vec<ExtractionFailure>>;

/// Retry a quarantined extraction (re-enqueue the job).
pub fn retry_failure(conn, failure_id, now) -> Result<()>;
```

### 13.2 Failure type

```rust
pub struct ExtractionFailure {
    pub id: i64,
    pub episode_id: String,
    pub extractor_id: String,
    pub raw_response: String,
    pub errors_json: String,    // Vec<ValidationError> serialized
    pub created_at: Timestamp,
}
```

Invalid claims never reach `beliefs`. They are recorded with their raw response
and typed errors, browsable and re-runnable. A bad batch never blocks a good one
(§7.4 repair loop: partial acceptance).

---

## 14. Eval harness + golden corpus

### 14.1 Golden corpus

~50 labeled episodes (M3 start; ~200 is the §14.1 target, grown incrementally):

- **Shapes:** note (markdown), document, agent-trace.
- **Languages:** English and Korean (bilingual — §14.1).
- **Annotations:** entities (surface, type, span), statements (predicate, subject,
  object, polarity), validity intervals.
- **Questions:** ~20 with reference answers and required supporting episodes.

Stored as JSON fixtures in `eval/corpus/{en,ko}/`. Each fixture:

```json
{
  "id": "en-note-001",
  "content": "Alice started working at Acme Corp in January 2024...",
  "source_kind": "note",
  "expected_entities": [
    { "surface": "Alice", "type": "Person", "span": [0, 5] },
    { "surface": "Acme Corp", "type": "Organization", "span": [31, 40] }
  ],
  "expected_claims": [
    {
      "predicate": "employed_by",
      "subject": { "surface": "Alice", "type": "Person" },
      "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
      "valid_from": "2024-01-01T00:00:00Z",
      "polarity": "affirm"
    }
  ]
}
```

### 14.2 Metrics (§14.2)

```rust
// eval/src/metrics.rs

pub struct EvalMetrics {
    pub fabricated_entity_rate: f64,  // target: 0.00 (structural hard zero)
    pub statement_precision: f64,     // target: ≥ 0.90
    pub statement_recall: f64,        // target: ≥ 0.70
    pub resolution_f1: f64,           // target: ≥ 0.92
    pub wrong_merge_rate: f64,        // target: ≤ 0.01
}

impl EvalMetrics {
    /// Compare extracted assertions against golden-corpus annotations.
    pub fn from_extraction(actual: &[ExtractedFact], expected: &[ExpectedFact]) -> Self;
}

pub struct ExtractedFact {
    pub subject_surface: String,
    pub predicate: String,
    pub object_repr: String,
    pub episode_id: String,
}
```

### 14.3 Fast suite (CI)

```bash
cargo run -p oxibrain-cli -- eval --suite fast
```

- Uses `FakeLlmPort` with fixture-replayed responses (no network).
- Runs on every PR.
- Asserts metrics meet §14.2 regression gates (block on > 2-3pp regression).
- Deterministic: same fixtures → same results.

### 14.4 Full suite (manual / nightly)

```bash
cargo run -p oxibrain-cli -- eval --suite full
```

- Uses a live provider (`oxibrain-llm-http`).
- Runs nightly and on extractor changes.
- Records metrics for trend tracking.
- Not a CI gate (requires API key + network).

### 14.5 Confidence calibration

The eval harness measures per-extractor precision/recall. The calibration
multiplier is derived:

```rust
/// Derive a calibration multiplier from eval metrics.
/// Higher precision → higher multiplier (trust the extractor more).
pub fn derive_calibration(metrics: &EvalMetrics) -> f32 {
    // Start conservative. Scale by precision, penalize by fabricated-entity rate.
    let base = 0.8;
    let precision_factor = metrics.statement_precision as f32;
    let fabrication_penalty = 1.0 - metrics.fabricated_entity_rate as f32;
    (base * precision_factor * fabrication_penalty).clamp(0.1, 2.0)
}
```

The calibration table is stored in the `meta` table (JSON) and applied during
the confidence fold (§6.5). An unmeasured extractor gets the conservative prior
of 0.8.

---

## 15. Budget measurement (deferred from M2)

The M2 bench suite compiles but numbers were not measured. M3 runs `cargo bench`
and records §13.2 numbers:

| Operation | p95 budget | Measurement source |
|---|---|---|
| declaration write | < 5 ms | `declaration_write` bench |
| `get_entity` | < 10 ms | `get_entity` bench |
| hybrid query (top 20) | < 80 ms | `hybrid_query_top20` bench |
| traversal, depth 3, ≤256 nodes | < 100 ms | `traversal_depth3_256` bench |
| `assemble_context` (3K tokens) | < 150 ms | `assemble_context_3k` bench |
| reproject from cache (whole store) | < 5 min | `reproject_from_cache` bench |
| cold start (index load) | < 2 s | `cold_start_index_load` bench |

Each budget may be revised **once** with measurement + reason recorded in
DESIGN.md §13.2 (D16). After revision, it becomes a regression gate.

---

## 16. Schema changes

**None.** All M3 tables already exist in v1.sql (M0 created them with foresight):

| Table | Used by | Status |
|---|---|---|
| `extractions` | Extraction cache | ✅ exists (PK: episode_id, extractor_id) |
| `ingest_jobs` | Job queue | ✅ exists (state, attempts, lease_until) |
| `extraction_failures` | Quarantine | ✅ exists (raw_response, errors_json) |
| `summaries` | Consolidation/community text cache | ✅ exists (scope_kind, member_set_hash, extractor_id) |
| `episode_links` | Derived episode → sources | ✅ exists (from_episode, to_episode, rel) |

No migration is needed. `LEDGER_SCHEMA_VERSION` stays at 3.

Calibration data is stored as JSON in the `meta` table (key:
`calibration_table`), not a new table.

---

## 17. Facade API (oxibrain/src/lib.rs)

```rust
impl Brain {
    // --- Extraction ---

    /// Ingest a note episode and enqueue an extraction job.
    /// Returns the episode id.
    pub async fn ingest(
        &self, space: &str, content: String, source: SourceRef,
    ) -> Result<String, BrainError>;

    /// Process pending extraction jobs (batch). Returns summary.
    pub async fn extract_pending(
        &self, space: &str, config: &ExtractorConfig,
    ) -> Result<ExtractSummary, BrainError>;

    /// Extract a single episode synchronously (realtime profile).
    pub async fn extract_one(
        &self, space: &str, episode_id: &str, config: &ExtractorConfig,
    ) -> Result<ExtractSummary, BrainError>;

    /// Re-extract all episodes with a new extractor config.
    pub async fn reextract(
        &self, space: &str, config: &ExtractorConfig,
    ) -> Result<ExtractSummary, BrainError>;

    /// Query job queue status.
    pub async fn job_status(&self, space: &str) -> Result<Vec<JobStatus>, BrainError>;

    /// List extraction failures (quarantine).
    pub async fn extraction_failures(
        &self, space: Option<&str>,
    ) -> Result<Vec<ExtractionFailure>, BrainError>;

    // --- Consolidation ---

    /// Consolidate related episodes into Derived episodes with cached summaries.
    pub async fn consolidate(
        &self, space: &str, config: &ExtractorConfig,
    ) -> Result<Vec<String>, BrainError>;

    /// Generate community summary text (cached).
    pub async fn summarize_communities(
        &self, space: &str, config: &ExtractorConfig,
    ) -> Result<usize, BrainError>;
}
```

All extraction methods follow the pattern established in M2: LLM calls run in
`spawn_blocking` tasks off the actor; DB writes are short `WriteOp`s on the
actor. Methods that need the LLM return `Err(BrainError::Config(...))` if no LLM
port is configured.

---

## 18. Deviations from DESIGN.md

| # | Deviation | DESIGN says | M3 does | Reason |
|---|---|---|---|---|
| D1 | Schema constrains structure, validator constrains semantics | §7.4: "schema-forced" | JSON Schema constrains predicate names + structure; validator enforces predicate↔type matching, cardinality, spans, verbatim | JSON Schema `if/then` conditionals are brittle and model-dependent for ~40 predicates. The split is cleaner: schema does structure, validator does semantics. Both are registry-generated (P4). |
| D2 | Dense embeddings deferred to M4 | M2 spec §3.2: "dense embeddings in M3" | TF-IDF remains the default; `oxibrain-embed-local` + sqlite-vec + HNSW defer to M4 | DESIGN §17 M3 scope does not list dense embeddings. GGUF runtime is a heavy native dependency. The M3 exit criteria (§14.2 gates) can be met with TF-IDF on a small corpus. Retrieval recall@10 is a budget that can be revised once (D16). |
| D3 | Core stays pure | §15: "core may depend on store" | Core defines extraction types + pure fns (schema, validator, prompt); store orchestrates | Consistent with M1/M2. Core gaining store deps is M5+. |
| D4 | Golden corpus starts at ~50, not ~200 | §14.1: "~200 labeled episodes" | M3 ships ~50; grows incrementally | ~200 is the target; M3 establishes the harness and CI gates with a smaller corpus. The regression gates bind from the first measurement (§14.2). |
| D5 | Extraction runs LLM off-actor, not on writer thread | §13.1: "long work runs off the actor" | Brain facade orchestrates: claim [WriteOp] → LLM [spawn_blocking] → project [WriteOp] | The writer actor runs all writes in transactions (§7.2). An LLM call inside a transaction would block all readers and writers for seconds-minutes. |
| D6 | Community summaries cached in `summaries` table, not as Derived episodes | §9.4: "summaries are Derived episodes" | Community summary text cached in `summaries`; consolidation summaries ARE Derived episodes | Community summaries are per-cluster text, not per-episode. Storing them in `summaries` (the existing cache table) avoids creating synthetic episodes for each community. Consolidation summaries (episode clusters) are Derived episodes with proper provenance. |

---

## 19. Open questions (M3 defaults)

1. **Byte span accuracy from the LLM.** LLMs may provide inaccurate byte offsets.
   *Default: the validator rejects out-of-bounds and non-verbatim spans. The
   repair loop gives one retry. If spans are consistently wrong, fall back to
   substring search: find the surface form in the content and compute the span.
   Revisit with eval data.*

2. **Cross-chunk coreference.** Long episodes are chunked with overlap (§7.4).
   *Default: M3 processes episodes as single chunks (no chunking yet). Chunking
   arrives when eval data shows quality degradation on long episodes. Entity
   resolution handles cross-chunk coreference at the resolution stage, not in
   the prompt.*

3. **FTS5 tokenizer for Korean.** `porter unicode61` doesn't handle CJK word
   boundaries. *Default: `porter unicode61` for M3; add bigram tokenization if
   eval shows poor Korean recall. The golden corpus includes Korean episodes to
   surface this.*

4. **Calibration stability.** The calibration multiplier changes as eval data
   accumulates. *Default: recalibrate on `eval --suite full` runs; store the
   latest in `meta`. Old assertions keep their original confidence (fold is
   idempotent for existing assertions).*

5. **Consolidation trigger.** When does consolidation run? *Default: on-demand
   (`oxibrain consolidate`). A nightly schedule is an M4 daemon concern.*

6. **Multiple extractors coexisting.** Two extractors produce assertions for the
   same episode. *Default: both coexist (different `extractor_id` → different
   assertions). The fold treats them as corroboration (distinct episodes = 1, but
   distinct extractors on the same episode = still 1 episode's evidence). This
   is the DESIGN §6.5 definition: corroboration saturates in distinct **episodes**,
   not extractors. An extractor promoting/demoting is a config change.*

---

## 20. Sub-session breakdown

| Sub-session | Scope | Est. commits | Difficulty |
|---|---|---|---|
| **M3a** | Job queue lifecycle; `oxibrain-llm-http` crate (Anthropic + OpenAI); `FakeLlmPort`; `ExtractorConfig` + `ExtractorId`; `Brain::ingest` creates job; `Brain::extract_pending` skeleton | 4–5 | 🔴 |
| **M3b** | `schema_from_registry`; `build_extraction_prompt`; `Claim`/`ExtractionResponse` types; extraction pipeline (LLM call → cache → parse → project); `project_extraction`; re-extraction; reproject extension (extraction replay) | 5–6 | 🔴 |
| **M3c** | `validate_claims`; repair loop; quarantine (`record_failure`, `list_failures`); consolidation (cluster → summarize → Derived); community summary text (cached) | 4–5 | 🟡 |
| **M3d** | Golden corpus fixtures; eval runner (`fast` suite); metrics (precision/recall/F1); confidence calibration; budget measurement (run bench, record §13.2); CI gates; §14.2 regression tests | 4–5 | 🟡 |

---

End of spec. Read this + `doc/DESIGN.md` §5.3 (derived episodes), §7 (extraction),
§8 (identity), §12.3 (sampling), §13.2 (budgets), §14 (eval), §17 (M3) + the
M2→M3 handoff (`docs/superpowers/handoffs/2026-08-11-m2-to-m3.md`) — then proceed
to the implementation plan.
