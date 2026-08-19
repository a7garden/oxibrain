//! Knowledge domain types (DESIGN §5.4). Entities, statements, assertions,
//! mentions, beliefs — the projection types derived from the ledger.

use crate::types::TrustTier;
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

pub type EntityId = String;
pub type EntityKeyId = String;
pub type StatementId = String;
pub type AssertionId = String;
pub type MentionId = String;

pub type EntityTypeRef = String;
pub type PredicateRef = String;

/// Opaque, permanent identity. Names live in EntityKey, not here (P3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub space: String,
    pub ty: EntityTypeRef,
    pub canonical_key: Option<EntityKeyId>,
    pub created_at: Timestamp,
    pub merged_into: Option<EntityId>,
}

/// A (type, normalized name) handle. Aliases are additional keys on one entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityKey {
    pub id: EntityKeyId,
    pub space: String,
    pub entity: EntityId,
    pub ty: EntityTypeRef,
    pub normalized: String,
    pub surface: String,
    pub origin: KeyOrigin,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyOrigin {
    Extracted,
    UserDeclared,
    Imported,
}

impl KeyOrigin {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Extracted => "extracted",
            Self::UserDeclared => "user_declared",
            Self::Imported => "imported",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "extracted" => Some(Self::Extracted),
            "user_declared" => Some(Self::UserDeclared),
            "imported" => Some(Self::Imported),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMerge {
    pub id: String,
    pub loser: EntityId,
    pub winner: EntityId,
    pub decided_by: MergeDecision,
    pub provenance: String,
    pub evidence: Vec<MentionId>,
    pub decided_at: Timestamp,
    pub undone_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MergeDecision {
    Rule { score: f64 },
    User,
    Import,
}

impl MergeDecision {
    /// (decided_by column, score column) for persistence.
    pub fn db_columns(&self) -> (&'static str, Option<f64>) {
        match self {
            Self::Rule { score } => ("rule", Some(*score)),
            Self::User => ("user", None),
            Self::Import => ("import", None),
        }
    }
    pub fn parse_db(kind: &str, score: Option<f64>) -> Option<Self> {
        match kind {
            "rule" => Some(Self::Rule {
                score: score.unwrap_or(0.0),
            }),
            "user" => Some(Self::User),
            "import" => Some(Self::Import),
            _ => None,
        }
    }
}

/// An atemporal proposition. Content-addressed → deduplicated by construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Statement {
    pub id: StatementId,
    pub space: String,
    pub subject: EntityId,
    pub predicate: PredicateRef,
    pub object: Object,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Object {
    Entity(EntityId),
    Literal(TypedValue),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TypedValue {
    Text(String),
    Date(String),
    DateTime(String),
    Quantity { value: f64, unit: String },
    Number(f64),
    Bool(bool),
    Enum(String),
}

/// "Episode E, via extractor R, claimed S held over I, with confidence c."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assertion {
    pub id: AssertionId,
    pub statement: StatementId,
    pub episode: String,
    pub extractor: Option<String>,
    pub polarity: Polarity,
    pub claimed_from: Timestamp,
    pub claimed_to: Timestamp,
    pub confidence: f32,
    pub recorded_at: Timestamp,
    pub retracted_at: Option<Timestamp>,
    /// Trust tier of the supporting episode at ingest time.
    #[serde(default)]
    pub trust: TrustTier,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    Affirm,
    Deny,
}

impl Polarity {
    pub fn as_db(&self) -> i64 {
        match self {
            Self::Affirm => 1,
            Self::Deny => -1,
        }
    }
    pub fn parse_db(v: i64) -> Option<Self> {
        match v {
            1 => Some(Self::Affirm),
            -1 => Some(Self::Deny),
            _ => None,
        }
    }
}

/// The verbatim text this assertion came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    pub id: MentionId,
    pub assertion: AssertionId,
    pub role: MentionRole,
    pub surface: String,
    pub span: (u32, u32),
    pub resolved_to: Option<EntityId>,
    pub method: ResolutionMethod,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionRole {
    Subject,
    Object,
}

impl MentionRole {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Object => "object",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "subject" => Some(Self::Subject),
            "object" => Some(Self::Object),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ResolutionMethod {
    ExactKey,
    Alias,
    Lexical { score: f64 },
    Embedding { score: f64 },
    New,
    User,
}

impl ResolutionMethod {
    /// (method column, score column) for persistence. score is NULL for non-scoring methods.
    pub fn db_columns(&self) -> (&'static str, Option<f64>) {
        match self {
            Self::ExactKey => ("exact_key", None),
            Self::Alias => ("alias", None),
            Self::Lexical { score } => ("lexical", Some(*score)),
            Self::Embedding { score } => ("embedding", Some(*score)),
            Self::New => ("new", None),
            Self::User => ("user", None),
        }
    }
    pub fn parse_db(method: &str, score: Option<f64>) -> Option<Self> {
        match method {
            "exact_key" => Some(Self::ExactKey),
            "alias" => Some(Self::Alias),
            "lexical" => Some(Self::Lexical {
                score: score.unwrap_or(0.0),
            }),
            "embedding" => Some(Self::Embedding {
                score: score.unwrap_or(0.0),
            }),
            "new" => Some(Self::New),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// Current-slice cache of the temporal fold. Fully derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub statement: StatementId,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub support: Support,
    pub confidence: f32,
    pub status: BeliefStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Support {
    pub affirm_count: u32,
    pub deny_count: u32,
    pub distinct_episodes: u32,
    /// Sorted by TrustTier ordinal (Trusted < SemiTrusted < Untrusted) for
    /// deterministic serialization. Each entry: (tier, count of episodes).
    pub trust_weights: Vec<(TrustTier, u32)>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeliefStatus {
    Active,
    Superseded,
    Contradicted,
    Retracted,
}

impl BeliefStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Contradicted => "contradicted",
            Self::Retracted => "retracted",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "superseded" => Some(Self::Superseded),
            "contradicted" => Some(Self::Contradicted),
            "retracted" => Some(Self::Retracted),
            _ => None,
        }
    }
}

/// Canonical string representation of an Object for StatementId hashing (DESIGN §5.6).
/// Prefix-based to avoid JSON float formatting issues.
pub fn object_repr(object: &Object) -> String {
    match object {
        Object::Entity(id) => format!("e:{id}"),
        Object::Literal(TypedValue::Text(s)) => format!("t:{s}"),
        Object::Literal(TypedValue::Date(s)) => format!("d:{s}"),
        Object::Literal(TypedValue::DateTime(s)) => format!("dt:{s}"),
        Object::Literal(TypedValue::Number(n)) => format!("n:{}", canonical_f64(*n)),
        Object::Literal(TypedValue::Bool(b)) => format!("b:{b}"),
        Object::Literal(TypedValue::Quantity { value, unit }) => {
            format!("q:{}:{unit}", canonical_f64(*value))
        }
        Object::Literal(TypedValue::Enum(s)) => format!("en:{s}"),
    }
}

/// Canonical f64: avoid -0.0, normalize integer-valued floats.
fn canonical_f64(n: f64) -> String {
    if n == 0.0 {
        "0".to_string()
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Canonical string representation of a claim for AssertionId hashing.
/// Uses f32::to_bits() for deterministic float representation.
pub fn claim_repr(
    polarity: Polarity,
    claimed_from: Timestamp,
    claimed_to: Timestamp,
    confidence: f32,
) -> String {
    format!(
        "{}:{}:{}:{}",
        match polarity {
            Polarity::Affirm => "a",
            Polarity::Deny => "d",
        },
        claimed_from.millis(),
        claimed_to.millis(),
        confidence.to_bits(),
    )
}
