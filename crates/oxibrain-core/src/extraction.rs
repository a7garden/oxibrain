//! Extraction types and pure functions (DESIGN §7). The LLM arrives in M3, but
//! non-determinism is contained: schema generation, prompt building, and validation
//! are all pure functions of the predicate registry. The LLM call itself is the
//! only non-deterministic step, and its output is cached (§7.3).
//!
//! This module defines:
//! - Types: `Claim`, `MentionRef`, `ClaimObject`, `ExtractionResponse`, `ExtractSummary`
//! - Identity: `ExtractorConfig`, `ExtractMechanism`
//! - Schema: `schema_from_registry` (pure fn of the registry)
//! - Prompt: `build_extraction_prompt` (pure fn of the registry)
//! - Validation: `validate_claims` (pure fn of claims + content + registry)

use crate::knowledge::Polarity;
use crate::registry::{LiteralType, ObjectKind, PredicateDef};
use serde::{Deserialize, Serialize};

// ─── Extractor identity (§7.5) ───────────────────────────────────────────────

/// Configuration for an extractor: model, prompt version, mechanism.
/// Hashes to an ExtractorId (§7.5). Only the registry MAJOR version invalidates
/// the cache (D8); adding a predicate is a minor bump and does not force re-extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractorConfig {
    pub model_id: String,
    pub prompt_version: u32,
    pub registry_major: u32,
    pub mechanism: ExtractMechanism,
    pub max_tokens: u32,
    /// blake3 hex digest of the model weights. Changing weights must change
    /// the extractor id (§9.5) — a silent quality change would poison the
    /// extraction cache.
    #[serde(default)]
    pub model_digest: Option<String>,
}

/// How structured output is enforced (§7.4). Recorded in the ExtractorId hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractMechanism {
    /// Provider-native JSON Schema structured output (OpenAI).
    JsonSchema,
    /// Forced tool call (Anthropic).
    ToolCall,
    /// GBNF grammar-constrained decoding (local GGUF path, §9.4 D28). The
    /// grammar is generated from the predicate registry (P4).
    Grammar,
    /// JSON mode without schema enforcement (weakest; validator is the only gate).
    JsonMode,
}

impl ExtractorConfig {
    /// ExtractorId = blake3(model_id, prompt_version, registry_major, mechanism[, model_digest]).
    /// Only the MAJOR registry version invalidates the cache (D8).
    /// The model digest — when present — invalidates it on weight changes (§9.5).
    pub fn id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.model_id.as_bytes());
        hasher.update(&self.prompt_version.to_le_bytes());
        hasher.update(&self.registry_major.to_le_bytes());
        hasher.update(&[self.mechanism as u8]);
        if let Some(digest) = &self.model_digest {
            hasher.update(digest.as_bytes());
        }
        hex::encode(hasher.finalize().as_bytes())
    }
}

// ─── Extraction types ────────────────────────────────────────────────────────

/// A reference to an entity mention in the episode text.
/// `surface` must appear verbatim at `[span.0, span.1)` (the fabricated-entity gate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MentionRef {
    pub surface: String,
    pub entity_type: String,
    pub span: (u32, u32),
}

/// The object of an extracted claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimObject {
    Entity {
        mention: MentionRef,
    },
    Literal {
        literal_type: String,
        value: String,
        span: (u32, u32),
    },
}

/// One extracted claim from the LLM. Maps to one assertion + mentions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub predicate: String,
    pub subject: MentionRef,
    pub object: ClaimObject,
    pub polarity: Polarity,
    /// Epoch millis. None = TIME_MIN (always true).
    #[serde(default)]
    pub valid_from: Option<i64>,
    /// Epoch millis. None = TIME_MAX (still true).
    #[serde(default)]
    pub valid_to: Option<i64>,
    pub confidence: f32,
}

/// The parsed LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResponse {
    pub claims: Vec<Claim>,
}

/// Summary of an extraction batch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractSummary {
    pub extracted: usize,
    pub quarantined: usize,
    pub episodes_done: usize,
    pub episodes_failed: usize,
}

/// Extraction budget limits (§7.6). The queue holds on exhaustion; it never drops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionBudget {
    pub max_concurrent: usize,
    pub max_episodes_per_batch: usize,
    pub max_tokens_per_episode: u32,
    pub max_repair_attempts: u32,
    pub lease_timeout_secs: u64,
}

impl Default for ExtractionBudget {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            max_episodes_per_batch: 50,
            max_tokens_per_episode: 8192,
            max_repair_attempts: 1,
            lease_timeout_secs: 300,
        }
    }
}

// ─── Validation ──────────────────────────────────────────────────────────────

/// Result of validating claims against the registry + episode content.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub valid: Vec<Claim>,
    pub invalid: Vec<(Claim, Vec<ValidationError>)>,
}

/// A validation error for a single claim (§7.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidationError {
    UnknownPredicate {
        predicate: String,
    },
    SubjectTypeMismatch {
        predicate: String,
        expected: Vec<String>,
        got: String,
    },
    ObjectTypeMismatch {
        predicate: String,
        expected: String,
        got: String,
    },
    MalformedLiteral {
        literal_type: String,
        value: String,
        reason: String,
    },
    SpanOutOfBounds {
        span: (u32, u32),
        content_len: usize,
    },
    SurfaceNotVerbatim {
        surface: String,
        span: (u32, u32),
        found: String,
    },
    ConfidenceOutOfRange {
        confidence: f32,
    },
}

/// Validate parsed claims against the registry + episode content (§7.4).
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
        let Some(pred_def) = predicates.iter().find(|p| p.name == claim.predicate) else {
            errors.push(ValidationError::UnknownPredicate {
                predicate: claim.predicate.clone(),
            });
            invalid.push((claim.clone(), errors));
            continue;
        };

        // 3. Subject type matches predicate.subject_types.
        if !pred_def
            .subject_types
            .iter()
            .any(|t| t == &claim.subject.entity_type)
        {
            errors.push(ValidationError::SubjectTypeMismatch {
                predicate: claim.predicate.clone(),
                expected: pred_def
                    .subject_types
                    .iter()
                    .map(|t| t.to_string())
                    .collect(),
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
                        expected: format!("{expected_lt:?}"),
                        got: literal_type.clone(),
                    });
                }
            }
            (
                ClaimObject::Literal {
                    literal_type,
                    value,
                    ..
                },
                ObjectKind::Enum(variants),
            ) => {
                if !variants.iter().any(|v| v == value) {
                    errors.push(ValidationError::ObjectTypeMismatch {
                        predicate: claim.predicate.clone(),
                        expected: format!("enum: {}", variants.join("|")),
                        got: value.clone(),
                    });
                }
                let _ = literal_type; // enum values are strings; type is not constrained further
            }
            (ClaimObject::Entity { .. }, ObjectKind::Literal(_))
            | (ClaimObject::Entity { .. }, ObjectKind::Enum(_))
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

        // 5-7. Spans and surfaces. Model offsets drift on multibyte text and
        // casing; repair before rejecting. The fabricated-entity gate still
        // holds: a claim survives only when each surface is literally present
        // in the content at the (possibly corrected) span.
        let mut repaired = claim.clone();
        if !resolve_mention(&mut repaired.subject, content) {
            errors.push(ValidationError::SurfaceNotVerbatim {
                surface: claim.subject.surface.clone(),
                span: claim.subject.span,
                found: String::new(),
            });
        }
        if let ClaimObject::Entity { mention } = &mut repaired.object {
            if !resolve_mention(mention, content) {
                errors.push(ValidationError::SurfaceNotVerbatim {
                    surface: mention.surface.clone(),
                    span: mention.span,
                    found: String::new(),
                });
            }
        }
        if let ClaimObject::Literal { span, .. } = &repaired.object {
            check_span(span, content, &mut errors);
        }

        if errors.is_empty() {
            valid.push(repaired);
        } else {
            invalid.push((claim.clone(), errors));
        }
    }

    ValidationResult { valid, invalid }
}

fn check_span(span: &(u32, u32), content: &str, errors: &mut Vec<ValidationError>) {
    let len = content.len();
    if span.0 as usize >= len || span.1 as usize > len || span.0 >= span.1 {
        errors.push(ValidationError::SpanOutOfBounds {
            span: *span,
            content_len: len,
        });
    }
}

/// Resolve a mention against the content, repairing span-interpretation drift
/// in place. Returns `true` when the surface is verbatim-present at the span.
///
/// Repair ladder — every step keeps the fabricated-entity gate intact: the
/// bytes at the (final) span must spell the surface. Relocating a surface to
/// a *different* span is deliberately NOT done: the injection suite
/// (oxibrain-store/tests/injection_suite.rs) requires that a span citing the
/// wrong bytes is rejected even when the surface occurs elsewhere.
/// 1. Exact byte span.
/// 2. Char-index span — models count chars on multibyte (Korean/CJK) text.
/// 3. Casing drift at the same span — canonicalize surface to the source text.
fn resolve_mention(m: &mut MentionRef, content: &str) -> bool {
    let (a, b) = (m.span.0 as usize, m.span.1 as usize);
    // 1. Exact byte span.
    if content.get(a..b) == Some(m.surface.as_str()) {
        return true;
    }
    // 2. Char-index span.
    if let Some(range) = char_span_to_bytes(content, a, b) {
        if content.get(range.clone()) == Some(m.surface.as_str()) {
            m.span = (range.start as u32, range.end as u32);
            return true;
        }
    }
    // 3. Casing drift at the same span: the content is the source of truth.
    if let Some(found) = content.get(a..b) {
        if found.eq_ignore_ascii_case(m.surface.as_str()) {
            m.surface = found.to_string();
            return true;
        }
    }
    false
}

/// Convert a (char_index_start, char_index_end) span to a byte range.
/// Returns `None` when the indices don't address this content.
fn char_span_to_bytes(content: &str, a: usize, b: usize) -> Option<std::ops::Range<usize>> {
    if b < a {
        return None;
    }
    let mut start = None;
    let mut end = None;
    let mut idx = 0usize;
    for (bi, _) in content.char_indices() {
        if idx == a {
            start = Some(bi);
        }
        if idx == b {
            end = Some(bi);
            break;
        }
        idx += 1;
    }
    // `b` may equal the char count — the range then runs to the end.
    let end = end.or((idx == b).then_some(content.len()))?;
    Some(start?..end)
}

fn literal_type_matches(given: &str, expected: &LiteralType) -> bool {
    match expected {
        LiteralType::Text => given == "text",
        LiteralType::Date => given == "date",
        LiteralType::DateTime => given == "datetime",
        LiteralType::Number => given == "number",
        LiteralType::Bool => given == "bool",
        LiteralType::Quantity { .. } => given == "quantity",
    }
}

// ─── Schema generation (§6.1) ────────────────────────────────────────────────

/// Generate the extraction JSON Schema from the predicate registry (P4).
/// Pure function: same registry → same schema. The schema constrains structure;
/// semantic rules (predicate↔type matching) are enforced by `validate_claims`.
pub fn schema_from_registry(predicates: &[PredicateDef]) -> serde_json::Value {
    let pred_names: Vec<&str> = predicates.iter().map(|p| p.name.as_str()).collect();
    let entity_types: Vec<&str> = predicates
        .iter()
        .flat_map(|p| {
            let subjects = p.subject_types.iter().map(|t| t.as_str());
            let objects = match &p.object_kind {
                ObjectKind::Entity(t) => vec![t.as_str()],
                _ => vec![],
            };
            subjects.chain(objects)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let mention_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "surface": { "type": "string", "description": "Verbatim text from the episode" },
            "entity_type": { "type": "string", "enum": entity_types },
            "span": {
                "type": "array",
                "items": { "type": "integer" },
                "minItems": 2,
                "maxItems": 2,
                "description": "Byte offset [start, end) into the episode text"
            }
        },
        "required": ["surface", "entity_type", "span"]
    });

    let object_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": ["entity", "literal"] }
        },
        "required": ["kind"],
        "oneOf": [
            {
                "properties": {
                    "kind": { "const": "entity" },
                    "mention": mention_schema.clone()
                },
                "required": ["mention"]
            },
            {
                "properties": {
                    "kind": { "const": "literal" },
                    "literal_type": { "type": "string", "enum": ["text", "date", "datetime", "number", "bool", "quantity"] },
                    "value": { "type": "string" },
                    "span": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "minItems": 2,
                        "maxItems": 2
                    }
                },
                "required": ["literal_type", "value", "span"]
            }
        ]
    });

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
                        "subject": mention_schema,
                        "object": object_schema,
                        "polarity": {
                            "type": "string",
                            "enum": ["affirm", "deny"]
                        },
                        "valid_from": {
                            "type": ["integer", "null"],
                            "description": "Epoch millis, or null for 'always'"
                        },
                        "valid_to": {
                            "type": ["integer", "null"],
                            "description": "Epoch millis, or null for 'still true'"
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

/// Build a GBNF alternation of JSON string literals for an enum.
/// Produces: `"\"value1\"" | "\"value2\"" | ...`
/// which in GBNF matches one of the JSON strings "value1", "value2", ...
fn enum_alternation(values: &[&str]) -> String {
    values
        .iter()
        .map(|v| format!("\"\\\"{v}\\\"\""))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Generate a GBNF grammar from the predicate registry (§9.4, D28, P4).
///
/// Sibling of [`schema_from_registry`] — one registry, two consumers.
/// The grammar constrains structure and enum values; semantic rules
/// (confidence range, span validity, type matching) are enforced by
/// [`validate_claims`].
///
/// Any JSON matching this grammar parses into an [`ExtractionResponse`],
/// and any valid serialized [`ExtractionResponse`] is accepted by the grammar.
/// The grammar enforces a canonical key order; serde deserializes by field
/// name, so the order is irrelevant on the consumer side.
pub fn grammar_from_registry(predicates: &[PredicateDef]) -> String {
    // Collect enum values — same logic as schema_from_registry.
    let pred_names: Vec<&str> = predicates.iter().map(|p| p.name.as_str()).collect();
    let entity_types: Vec<&str> = predicates
        .iter()
        .flat_map(|p| {
            let subjects = p.subject_types.iter().map(|t| t.as_str());
            let objects = match &p.object_kind {
                ObjectKind::Entity(t) => vec![t.as_str()],
                _ => vec![],
            };
            subjects.chain(objects)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let pred_alts = enum_alternation(&pred_names);
    let etype_alts = enum_alternation(&entity_types);

    // GBNF (GGML BNF) for llama.cpp. See llama.cpp grammars/README.md.
    // {{ and }} are format! escapes for literal { and }.
    // GBNF (GGML BNF) for llama.cpp. Each rule on a single line — the
    // parser treats newlines as rule separators. See llama.cpp grammars/README.md.
    format!(
        r#"root ::= ws "{{" ws "\"claims\"" ws ":" ws "[" ws claims ws "]" ws "}}"
claims ::= (claim (ws "," ws claim)*)?
claim ::= "{{" ws "\"predicate\"" ws ":" ws predicate ws "," ws "\"subject\"" ws ":" ws mention ws "," ws "\"object\"" ws ":" ws object-union ws "," ws "\"polarity\"" ws ":" ws polarity ws "," ws valid-from-opt valid-to-opt "\"confidence\"" ws ":" ws number ws "}}"
valid-from-opt ::= ("\"valid_from\"" ws ":" ws temporal-val ws "," ws)?
valid-to-opt ::= ("\"valid_to\"" ws ":" ws temporal-val ws "," ws)?
temporal-val ::= "null" | integer
mention ::= "{{" ws "\"surface\"" ws ":" ws string ws "," ws "\"entity_type\"" ws ":" ws entity-type ws "," ws "\"span\"" ws ":" ws "[" ws integer ws "," ws integer ws "]" ws "}}"
object-union ::= entity-object | literal-object
entity-object ::= "{{" ws "\"kind\"" ws ":" ws "\"entity\"" ws "," ws "\"mention\"" ws ":" ws mention ws "}}"
literal-object ::= "{{" ws "\"kind\"" ws ":" ws "\"literal\"" ws "," ws "\"literal_type\"" ws ":" ws literal-type ws "," ws "\"value\"" ws ":" ws string ws "," ws "\"span\"" ws ":" ws "[" ws integer ws "," ws integer ws "]" ws "}}"
entity-type ::= {etype_alts}
literal-type ::= "\"text\"" | "\"date\"" | "\"datetime\"" | "\"number\"" | "\"bool\"" | "\"quantity\""
predicate ::= {pred_alts}
polarity ::= "\"affirm\"" | "\"deny\""
string ::= "\"" ([^"\\] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]))* "\"" ws
number ::= ("-"? ([0-9] | [1-9] [0-9]*)) ("." [0-9]+)? ([eE] [-+]? [0-9]+)? ws
integer ::= "-"? ([0-9] | [1-9] [0-9]*) ws
ws ::= [ \t\n]*
"#,
    )
}

/// Build the extraction system prompt from the registry (P4: no hard-coded predicates).
/// Pure function.
pub fn build_extraction_prompt(predicates: &[PredicateDef]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are a knowledge extraction engine. Extract structured claims from the given text. \
         Each claim references entities by their VERBATIM surface form and byte span in the text.\n\n\
         Available predicates:\n",
    );
    for p in predicates {
        let obj_desc = match &p.object_kind {
            ObjectKind::Entity(t) => format!("entity: {t}"),
            ObjectKind::Literal(lt) => format!("literal: {lt:?}"),
            ObjectKind::Enum(variants) => format!("enum: {}", variants.join("|")),
        };
        s.push_str(&format!("- {} ({}): {}\n", p.name, obj_desc, p.description));
        if !p.examples.is_empty() {
            s.push_str(&format!("  Examples: {}\n", p.examples.join("; ")));
        }
    }
    s.push_str(
        "\nReturn JSON matching the provided schema. For each entity mention, provide the surface \
         text exactly as it appears in the episode, its type, and the byte offset range \
         [start, end) where it appears in the text. Byte offsets are relative to the start of \
         the episode text (offset 0 = first byte).\n\n\
         Rules:\n\
         - Entity surfaces MUST appear verbatim in the text at the given byte span.\n\
         - Only use predicates from the list above.\n\
         - Subject and object types must match the predicate's definition.\n\
         - Set confidence to your confidence in the claim (0.0 to 1.0).\n\
         - Use valid_from/valid_to for time-bounded claims. Use null for 'always true' or 'still true'.\n",
    );
    s
}

// ─── Few-shot selection (§9.6, 10.8) ──────────────────────────────────────

/// A golden-corpus example for few-shot extraction (§9.6, 10.8).
/// The `text` is the episode content; `claims_json` is the expected
/// extraction output as a JSON string.
#[derive(Debug, Clone)]
pub struct FewShotExample {
    pub text: String,
    pub claims_json: String,
}

/// Select the k most similar golden episodes to the target text, using
/// character trigram Jaccard similarity (§9.6, 10.8). Language-independent
/// by construction (P11).
///
/// Pure function: same inputs → same selection.
pub fn few_shot_examples<'a>(
    target_text: &str,
    corpus: &'a [FewShotExample],
    k: usize,
) -> Vec<&'a FewShotExample> {
    if corpus.is_empty() || k == 0 {
        return Vec::new();
    }
    let target_shingles = oxibrain_index::shingles(target_text.to_lowercase().trim(), 3);
    let mut scored: Vec<(f64, &FewShotExample)> = corpus
        .iter()
        .map(|ex| {
            let ex_shingles = oxibrain_index::shingles(ex.text.to_lowercase().trim(), 3);
            let sim = oxibrain_index::jaccard(&target_shingles, &ex_shingles);
            (sim, ex)
        })
        .collect();
    // Sort by similarity descending; tie-break on text for determinism.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.text.cmp(&b.1.text))
    });
    scored.iter().take(k).map(|(_, ex)| *ex).collect()
}

/// Format selected few-shot examples as prompt text (§9.6, 10.8).
/// Injected into the system prompt before the target episode.
pub fn format_few_shot(examples: &[&FewShotExample]) -> String {
    if examples.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nHere are some examples of correct extraction:\n\n");
    for (i, ex) in examples.iter().enumerate() {
        out.push_str(&format!("Example {}:\n", i + 1));
        out.push_str(&format!("Input: {}\n", ex.text));
        out.push_str(&format!("Output: {}\n\n", ex.claims_json));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_id_deterministic() {
        let c = ExtractorConfig {
            model_id: "claude-sonnet-4-5".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::ToolCall,
            max_tokens: 8192,
            model_digest: None,
        };
        assert_eq!(c.id(), c.id());
    }

    #[test]
    fn grammar_mechanism_changes_extractor_id() {
        // GBNF-constrained decoding (local path, §9.4 D28) must be recorded
        // in the ExtractorId — different output mechanism, different cache key.
        let base = ExtractorConfig {
            model_id: "qwen2.5-1.5b-instruct".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::Grammar,
            max_tokens: 8192,
            model_digest: None,
        };
        assert_eq!(base.id(), base.id());
        let json_mode = ExtractorConfig {
            mechanism: ExtractMechanism::JsonSchema,
            ..base.clone()
        };
        assert_ne!(base.id(), json_mode.id());
    }

    #[test]
    fn resolve_mention_repairs_casing_but_not_location() {
        // Casing drift at the exact span → surface canonicalized to content.
        let content = "The user prefers Rust.";
        let mut m = MentionRef {
            surface: "the user".into(),
            entity_type: "person".into(),
            span: (0, 8),
        };
        assert!(resolve_mention(&mut m, content));
        assert_eq!(m.surface, "The user");

        // Wrong span is rejected even when the surface occurs verbatim
        // elsewhere — the injection suite relies on this (no relocation).
        let mut m = MentionRef {
            surface: "Rust".into(),
            entity_type: "technology".into(),
            span: (0, 4),
        };
        assert!(!resolve_mention(&mut m, content));
    }

    #[test]
    fn resolve_mention_rejects_fabricated_surface() {
        let content = "The user prefers Rust.";
        let mut m = MentionRef {
            surface: "Python".into(),
            entity_type: "technology".into(),
            span: (0, 6),
        };
        assert!(!resolve_mention(&mut m, content));
    }

    #[test]
    fn extractor_id_changes_with_model() {
        let base = ExtractorConfig {
            model_id: "a".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::JsonSchema,
            max_tokens: 4096,
            model_digest: None,
        };
        let diff = ExtractorConfig {
            model_id: "b".into(),
            ..base.clone()
        };
        assert_ne!(base.id(), diff.id());
    }

    #[test]
    fn extractor_id_changes_with_mechanism() {
        let base = ExtractorConfig {
            model_id: "a".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::JsonSchema,
            max_tokens: 4096,
            model_digest: None,
        };
        let diff = ExtractorConfig {
            mechanism: ExtractMechanism::ToolCall,
            ..base.clone()
        };
        assert_ne!(base.id(), diff.id());
    }

    #[test]
    fn extractor_id_changes_with_registry_major() {
        let base = ExtractorConfig {
            model_id: "a".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::JsonSchema,
            max_tokens: 4096,
            model_digest: None,
        };
        let diff = ExtractorConfig {
            registry_major: 2,
            ..base.clone()
        };
        assert_ne!(base.id(), diff.id());
    }

    #[test]
    fn extractor_id_changes_with_digest() {
        // §9.5: changing model weights must change the extractor id, or a
        // silent quality change would poison the extraction cache.
        let base = ExtractorConfig {
            model_id: "qwen2.5-1.5b".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::JsonSchema,
            max_tokens: 8192,
            model_digest: Some("abc123".into()),
        };
        let diff = ExtractorConfig {
            model_digest: Some("def456".into()),
            ..base.clone()
        };
        assert_ne!(
            base.id(),
            diff.id(),
            "weight change must invalidate ExtractorId"
        );

        // A missing digest must also differ (opt-in digest changes the id).
        let nodigest = ExtractorConfig {
            model_digest: None,
            ..base.clone()
        };
        assert_ne!(base.id(), nodigest.id());
    }

    #[test]
    fn schema_contains_all_predicates() {
        let schema = schema_from_registry(crate::registry::core_v1());
        let claims_items =
            &schema["properties"]["claims"]["items"]["properties"]["predicate"]["enum"];
        let names: Vec<String> = claims_items
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"works_on".to_string()));
        assert!(names.contains(&"employed_by".to_string()));
        assert!(names.contains(&"born_in".to_string()));
    }

    #[test]
    fn prompt_contains_predicate_descriptions() {
        let prompt = build_extraction_prompt(crate::registry::core_v1());
        assert!(prompt.contains("works_on"));
        assert!(prompt.contains("project"));
        assert!(prompt.contains("VERBATIM"));
    }

    fn make_claim(
        predicate: &str,
        subj_surface: &str,
        subj_type: &str,
        subj_span: (u32, u32),
        obj_surface: &str,
        obj_type: &str,
        obj_span: (u32, u32),
    ) -> Claim {
        Claim {
            predicate: predicate.into(),
            subject: MentionRef {
                surface: subj_surface.into(),
                entity_type: subj_type.into(),
                span: subj_span,
            },
            object: ClaimObject::Entity {
                mention: MentionRef {
                    surface: obj_surface.into(),
                    entity_type: obj_type.into(),
                    span: obj_span,
                },
            },
            polarity: Polarity::Affirm,
            valid_from: None,
            valid_to: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn validate_valid_claim() {
        let content = "Alice works on ProjectX at Acme Corp.";
        let claim = make_claim(
            "works_on",
            "Alice",
            "Person",
            (0, 5),
            "ProjectX",
            "Project",
            (15, 23),
        );
        let result = validate_claims(&[claim], content, crate::registry::core_v1());
        assert_eq!(result.valid.len(), 1);
        assert!(result.invalid.is_empty());
    }

    #[test]
    fn validate_unknown_predicate() {
        let content = "Alice works on ProjectX.";
        let claim = make_claim(
            "unknown_pred",
            "Alice",
            "Person",
            (0, 5),
            "ProjectX",
            "Project",
            (15, 23),
        );
        let result = validate_claims(&[claim], content, crate::registry::core_v1());
        assert!(result.valid.is_empty());
        assert_eq!(result.invalid.len(), 1);
    }

    #[test]
    fn validate_fabricated_entity_rejected() {
        let content = "Alice works on ProjectX.";
        // Surface "Bob" doesn't appear at span [0, 5) — the content there is "Alice".
        let claim = make_claim(
            "works_on",
            "Bob",
            "Person",
            (0, 5),
            "ProjectX",
            "Project",
            (15, 23),
        );
        let result = validate_claims(&[claim], content, crate::registry::core_v1());
        assert!(result.valid.is_empty());
        assert_eq!(result.invalid.len(), 1);
        assert!(matches!(
            result.invalid[0].1[0],
            ValidationError::SurfaceNotVerbatim { .. }
        ));
    }

    #[test]
    fn validate_span_out_of_bounds() {
        // Entity mentions with drifted spans are repaired (relocated) as long
        // as the surface is verbatim in the content; fabricated surfaces are
        // still rejected — the fabricated-entity gate.
        let content = "Alice works on ProjectX.";
        let claim = make_claim(
            "works_on",
            "Alice",
            "Person",
            (0, 5),
            "Zanzibar",
            "Project",
            (999, 1000),
        );
        let result = validate_claims(&[claim], content, crate::registry::core_v1());
        assert!(result.valid.is_empty());
    }

    #[test]
    fn validate_subject_type_mismatch() {
        let content = "Acme Corp employs Alice (0-5).";
        let claim = make_claim(
            "employed_by",
            "Acme Corp",
            "Organization",
            (0, 9),
            "Somewhere",
            "Organization",
            (17, 26),
        );
        let result = validate_claims(&[claim], content, crate::registry::core_v1());
        assert!(result.valid.is_empty());
        assert!(matches!(
            result.invalid[0].1[0],
            ValidationError::SubjectTypeMismatch { .. }
        ));
    }

    // ─── grammar_from_registry tests ──────────────────────────────────────

    #[test]
    fn grammar_smoke_has_rules() {
        let g = grammar_from_registry(crate::registry::core_v1());
        // Normalize whitespace so alignment in the template doesn't break matching.
        let norm: String = g.split_whitespace().collect::<Vec<_>>().join(" ");
        for rule in [
            "root",
            "claims",
            "claim",
            "mention",
            "object-union",
            "predicate",
            "entity-type",
            "polarity",
            "literal-type",
            "string",
            "number",
            "integer",
            "ws",
        ] {
            let needle = format!("{rule} ::=");
            assert!(
                norm.contains(&needle),
                "grammar missing rule definition for `{rule}`"
            );
        }
    }

    #[test]
    fn grammar_and_schema_agree_on_predicates() {
        let preds = crate::registry::core_v1();
        let grammar = grammar_from_registry(preds);
        let schema = schema_from_registry(preds);

        let schema_preds: std::collections::BTreeSet<String> =
            schema["properties"]["claims"]["items"]["properties"]["predicate"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();

        for name in &schema_preds {
            // The grammar must contain a GBNF literal for this predicate name.
            let needle = format!("\\\"{name}\\\"");
            assert!(
                grammar.contains(&needle),
                "grammar missing predicate `{name}` present in schema"
            );
        }
    }

    #[test]
    fn grammar_and_schema_agree_on_entity_types() {
        let preds = crate::registry::core_v1();
        let grammar = grammar_from_registry(preds);
        let schema = schema_from_registry(preds);

        let schema_types: std::collections::BTreeSet<String> = schema["properties"]["claims"]["items"]
            ["properties"]["subject"]["properties"]["entity_type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        for name in &schema_types {
            let needle = format!("\\\"{name}\\\"");
            assert!(
                grammar.contains(&needle),
                "grammar missing entity type `{name}` present in schema"
            );
        }
    }

    #[test]
    fn grammar_has_polarity_and_literal_type_enums() {
        let g = grammar_from_registry(crate::registry::core_v1());
        assert!(g.contains("\\\"affirm\\\""));
        assert!(g.contains("\\\"deny\\\""));
        for lt in ["text", "date", "datetime", "number", "bool", "quantity"] {
            assert!(
                g.contains(&format!("\\\"{lt}\\\"")),
                "grammar missing literal type `{lt}`"
            );
        }
    }

    #[test]
    fn grammar_valid_response_roundtrips_serde() {
        // A Claim serialized to JSON should round-trip through serde.
        // This is the structural half of the grammar/schema agreement: the
        // grammar generates the same structure that serde expects.
        let claim = make_claim(
            "works_on",
            "Alice",
            "Person",
            (0, 5),
            "ProjectX",
            "Project",
            (15, 23),
        );
        let resp = ExtractionResponse {
            claims: vec![claim],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ExtractionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.claims.len(), 1);
        assert_eq!(back.claims[0].predicate, "works_on");
    }

    #[test]
    fn grammar_has_optional_temporal_fields() {
        let g = grammar_from_registry(crate::registry::core_v1());
        assert!(g.contains("valid-from-opt"));
        assert!(g.contains("valid-to-opt"));
        // GBNF literals use backslash-escaped quotes.
        assert!(g.contains("\\\"valid_from\\\""));
    }
    #[test]
    fn grammar_supports_empty_claims() {
        // An empty claims array {"claims":[]} must be accepted.
        let g = grammar_from_registry(crate::registry::core_v1());
        // The claims rule uses ? to allow zero claims.
        assert!(g.contains("(claim (ws \",\" ws claim)*)?"));
    }

    // ── Few-shot selection (§9.6, 10.8) ────────────────────────────────

    #[test]
    fn few_shot_selects_most_similar() {
        let corpus = vec![
            FewShotExample {
                text: "Alice works at Acme.".into(),
                claims_json: r#"{"claims":[]}"#.into(),
            },
            FewShotExample {
                text: "Bob likes pizza.".into(),
                claims_json: r#"{"claims":[]}"#.into(),
            },
        ];
        let target = "Alice works at Globex.";
        let selected = few_shot_examples(target, &corpus, 1);
        assert_eq!(selected.len(), 1);
        assert!(
            selected[0].text.contains("Alice"),
            "should pick the most similar example, got: {}",
            selected[0].text
        );
    }

    #[test]
    fn few_shot_empty_corpus_returns_empty() {
        let corpus: Vec<FewShotExample> = vec![];
        let selected = few_shot_examples("any text", &corpus, 3);
        assert!(selected.is_empty());
    }

    #[test]
    fn few_shot_k_caps_results() {
        let corpus: Vec<FewShotExample> = (0..10)
            .map(|i| FewShotExample {
                text: format!("Sample text {i}."),
                claims_json: r#"{"claims":[]}"#.into(),
            })
            .collect();
        let selected = few_shot_examples("Sample text", &corpus, 3);
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn few_shot_format_includes_input_output() {
        let ex = FewShotExample {
            text: "Alice works at Acme.".into(),
            claims_json: r#"{"claims":[]}"#.into(),
        };
        let formatted = format_few_shot(&[&ex]);
        assert!(formatted.contains("Alice works at Acme"));
        assert!(formatted.contains(r#"{"claims":[]}"#));
    }

    #[test]
    fn few_shot_format_empty_returns_empty_string() {
        let formatted = format_few_shot(&[]);
        assert_eq!(formatted, "");
    }
}
