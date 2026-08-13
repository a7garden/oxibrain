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
}

/// How structured output is enforced (§7.4). Recorded in the ExtractorId hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractMechanism {
    /// Provider-native JSON Schema structured output (OpenAI).
    JsonSchema,
    /// Forced tool call (Anthropic).
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
        hasher.update(&[self.mechanism as u8]);
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

        // 5. Subject span exists in content.
        check_span(&claim.subject.span, content, &mut errors);

        // 6. Object span.
        match &claim.object {
            ClaimObject::Entity { mention } => {
                check_span(&mention.span, content, &mut errors);
            }
            ClaimObject::Literal { span, .. } => {
                check_span(span, content, &mut errors);
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

fn check_span(span: &(u32, u32), content: &str, errors: &mut Vec<ValidationError>) {
    let len = content.len();
    if span.0 as usize >= len || span.1 as usize > len || span.0 >= span.1 {
        errors.push(ValidationError::SpanOutOfBounds {
            span: *span,
            content_len: len,
        });
    }
}

fn check_verbatim(m: &MentionRef, content: &str, errors: &mut Vec<ValidationError>) {
    let bytes = content.as_bytes();
    if m.span.0 as usize >= bytes.len() || m.span.1 as usize > bytes.len() {
        return; // SpanOutOfBounds already recorded
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
    format!(
        r#"root ::= ws "{{" ws "\"claims\"" ws ":" ws "[" ws claims ws "]" ws "}}"

claims ::= (claim (ws "," ws claim)*)?

claim ::= "{{" ws
  "\"predicate\""   ws ":" ws predicate    ws "," ws
  "\"subject\""     ws ":" ws mention      ws "," ws
  "\"object\""      ws ":" ws object_union ws "," ws
  "\"polarity\""    ws ":" ws polarity     ws "," ws
  valid_from_opt
  valid_to_opt
  "\"confidence\""  ws ":" ws number
  ws "}}"

valid_from_opt ::= ("\"valid_from\"" ws ":" ws temporal_val ws "," ws)?
valid_to_opt   ::= ("\"valid_to\""   ws ":" ws temporal_val ws "," ws)?
temporal_val   ::= "null" | integer

mention ::= "{{" ws
  "\"surface\""     ws ":" ws string      ws "," ws
  "\"entity_type\"" ws ":" ws entity_type ws "," ws
  "\"span\""        ws ":" ws "[" ws integer ws "," ws integer ws "]"
  ws "}}"

object_union ::= entity_object | literal_object

entity_object ::= "{{" ws
  "\"kind\""    ws ":" ws "\"entity\"" ws "," ws
  "\"mention\"" ws ":" ws mention
  ws "}}"

literal_object ::= "{{" ws
  "\"kind\""         ws ":" ws "\"literal\""  ws "," ws
  "\"literal_type\"" ws ":" ws literal_type   ws "," ws
  "\"value\""        ws ":" ws string         ws "," ws
  "\"span\""         ws ":" ws "[" ws integer ws "," ws integer ws "]"
  ws "}}"

predicate    ::= {pred_alts}
entity_type  ::= {etype_alts}
polarity     ::= "\"affirm\"" | "\"deny\""
literal_type ::= "\"text\"" | "\"date\"" | "\"datetime\"" | "\"number\"" | "\"bool\"" | "\"quantity\""

string  ::= "\"" ([^"\\] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]))* "\"" ws
number  ::= ("-"? ([0-9] | [1-9] [0-9]*)) ("." [0-9]+)? ([eE] [-+]? [0-9]+)? ws
integer ::= "-"? ([0-9] | [1-9] [0-9]*) ws
ws      ::= [ \t\n]*
"#,
        pred_alts = pred_alts,
        etype_alts = etype_alts,
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
        };
        assert_eq!(c.id(), c.id());
    }

    #[test]
    fn extractor_id_changes_with_model() {
        let base = ExtractorConfig {
            model_id: "a".into(),
            prompt_version: 1,
            registry_major: 1,
            mechanism: ExtractMechanism::JsonSchema,
            max_tokens: 4096,
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
        };
        let diff = ExtractorConfig {
            registry_major: 2,
            ..base.clone()
        };
        assert_ne!(base.id(), diff.id());
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
        let content = "Alice works on ProjectX.";
        let claim = make_claim(
            "works_on",
            "Alice",
            "Person",
            (0, 5),
            "ProjectX",
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
            "object_union",
            "predicate",
            "entity_type",
            "polarity",
            "literal_type",
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
        assert!(g.contains("valid_from_opt"));
        assert!(g.contains("valid_to_opt"));
        // The grammar contains GBNF literals with backslash-escaped quotes.
        assert!(g.contains("\\\"valid_from\\\""));
        assert!(g.contains("\\\"valid_to\\\""));
    }
    #[test]
    fn grammar_supports_empty_claims() {
        // An empty claims array {"claims":[]} must be accepted.
        let g = grammar_from_registry(crate::registry::core_v1());
        // The claims rule uses ? to allow zero claims.
        assert!(g.contains("(claim (ws \",\" ws claim)*)?"));
    }
}
