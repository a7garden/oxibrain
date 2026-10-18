//! Predicate registry (DESIGN §5.5, P4). Predicate semantics — object type,
//! cardinality, temporality, invalidation, symmetry — declared here, not in prompts.
//! The registry drives the fold, the validator, and (in M3) the extraction schema.

use crate::knowledge::{EntityTypeRef, PredicateRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    Functional,
    MultiValued,
}

impl Cardinality {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Functional => "functional",
            Self::MultiValued => "multi_valued",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "functional" => Some(Self::Functional),
            "multi_valued" => Some(Self::MultiValued),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temporality {
    Static,
    Interval,
    Point,
}

impl Temporality {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Interval => "interval",
            Self::Point => "point",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "static" => Some(Self::Static),
            "interval" => Some(Self::Interval),
            "point" => Some(Self::Point),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Invalidation {
    Supersede,
    Coexist,
    ExplicitOnly,
}

impl Invalidation {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Supersede => "supersede",
            Self::Coexist => "coexist",
            Self::ExplicitOnly => "explicit_only",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "supersede" => Some(Self::Supersede),
            "coexist" => Some(Self::Coexist),
            "explicit_only" => Some(Self::ExplicitOnly),
            _ => None,
        }
    }
}

/// Entity types a predicate's object may take (registry minor 4+). Serializes
/// as a JSON array; deserialization also accepts the pre-4 single-string form
/// so `def_json` rows written by older registries keep loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityTypes(pub Vec<EntityTypeRef>);

impl<const N: usize> From<[&str; N]> for EntityTypes {
    fn from(types: [&str; N]) -> Self {
        Self(types.iter().map(|s| (*s).to_string()).collect())
    }
}

impl From<&str> for EntityTypes {
    fn from(t: &str) -> Self {
        Self(vec![t.to_string()])
    }
}

impl Serialize for EntityTypes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.0.iter())
    }
}

impl<'de> Deserialize<'de> for EntityTypes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EntityTypesVisitor;

        impl<'de> serde::de::Visitor<'de> for EntityTypesVisitor {
            type Value = EntityTypes;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an entity type string or an array of entity type strings")
            }

            fn visit_str<E>(self, v: &str) -> Result<EntityTypes, E>
            where
                E: serde::de::Error,
            {
                Ok(EntityTypes(vec![v.to_string()]))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<EntityTypes, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(t) = seq.next_element::<String>()? {
                    out.push(t);
                }
                Ok(EntityTypes(out))
            }
        }

        deserializer.deserialize_any(EntityTypesVisitor)
    }
}

/// What a predicate's object can be. Untagged; variant order is load-bearing:
/// `Literal` is tried first so literal type names ("text", "date", …) are not
/// swallowed by the string-accepting `Entity` visitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ObjectKind {
    Literal(LiteralType),
    Entity(EntityTypes),
    Enum { variants: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiteralType {
    Text,
    Date,
    DateTime,
    Quantity { unit: String },
    Number,
    Bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredicateDef {
    pub name: PredicateRef,
    #[serde(deserialize_with = "deserialize_object_kind", alias = "object")]
    pub object_kind: ObjectKind,
    #[serde(alias = "subjects")]
    pub subject_types: Vec<EntityTypeRef>,
    pub cardinality: Cardinality,
    pub temporality: Temporality,
    pub invalidation: Invalidation,
    pub symmetric: bool,
    #[serde(default)]
    pub inverse_of: Option<PredicateRef>,
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default)]
    pub deprecated_by: Option<PredicateRef>,
    /// §12.2 — predicate is "what the user wants in every answer". A predicate
    /// marked here is queried on every Profile render. Defaults to false.
    /// Bumping this on existing predicates is a minor-version event: existing
    /// cached extractions are unaffected (D21).
    #[serde(default)]
    pub profile_relevant: bool,
    /// §5.5 — confidence prior for this predicate. Defaults to 1.0 (normal
    /// evidence). Hearsay/uncertain predicates (e.g. `allegedly_employed_by`)
    /// carry a lower prior so the fold produces low-confidence beliefs
    /// automatically — no special-casing in the pipeline (P4).
    #[serde(default = "default_confidence_prior")]
    pub confidence_prior: f32,
}

/// Legacy `object_kind` shapes observed in historical `RegisterPredicate`
/// def_json (and the persisted `predicates` table):
/// - `"object": "literal:text"` (compact string scheme),
/// - `"object": {"kind": "literal", "literal_type": "text"}` (nested map).
///
/// Both map mechanically to the current untagged `ObjectKind`; anything else
/// falls through to the regular untagged deserialization.
fn deserialize_object_kind<'de, D>(deserializer: D) -> Result<ObjectKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    legacy_object_kind(&value)
        .ok_or_else(|| serde::de::Error::custom(format!("invalid object_kind: {value}")))
}

fn legacy_object_kind(value: &serde_json::Value) -> Option<ObjectKind> {
    match value {
        serde_json::Value::String(s) => {
            if let Some(lit) = s.strip_prefix("literal:") {
                let lit: LiteralType =
                    serde_json::from_value(serde_json::Value::String(lit.into())).ok()?;
                Some(ObjectKind::Literal(lit))
            } else if let Some(ty) = s.strip_prefix("entity:") {
                Some(ObjectKind::Entity(EntityTypes::from(ty)))
            } else {
                serde_json::from_value(value.clone()).ok()
            }
        }
        serde_json::Value::Object(map) => match map.get("kind")?.as_str()? {
            "literal" => {
                let lit: LiteralType =
                    serde_json::from_value(map.get("literal_type")?.clone()).ok()?;
                Some(ObjectKind::Literal(lit))
            }
            "entity" => {
                let types: EntityTypes =
                    serde_json::from_value(map.get("types").or_else(|| map.get("type")).cloned()?)
                        .ok()?;
                Some(ObjectKind::Entity(types))
            }
            _ => serde_json::from_value(value.clone()).ok(),
        },
        _ => serde_json::from_value(value.clone()).ok(),
    }
}

/// Default confidence prior for predicates (§5.5). Normal predicates do not
/// penalise belief confidence; hearsay predicates override this to < 1.0.
fn default_confidence_prior() -> f32 {
    1.0
}
/// Registry version for this ontology.
pub const CORE_V1_MAJOR: u32 = 1;
pub const CORE_V1_MINOR: u32 = 5;

static CORE_V1: std::sync::LazyLock<Vec<PredicateDef>> = std::sync::LazyLock::new(|| {
    vec![
        PredicateDef {
            name: "employed_by".into(),
            object_kind: ObjectKind::Entity("Organization".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Supersede,
            symmetric: false,
            inverse_of: None,
            description: "The organization that employs this person.".into(),
            examples: vec!["Alice is employed by Acme Corp".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "works_on".into(),
            object_kind: ObjectKind::Entity("Project".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "A project this person is currently working on.".into(),
            examples: vec!["Bob works on ProjectX".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "born_in".into(),
            object_kind: ObjectKind::Entity("Place".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Static,
            invalidation: Invalidation::Supersede,
            symmetric: false,
            inverse_of: None,
            description: "Where this person was born. A second value is a contradiction.".into(),
            examples: vec!["Alice was born in Seoul".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "full_name".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Supersede,
            symmetric: false,
            inverse_of: None,
            description: "The person's full legal name. A new value supersedes the old.".into(),
            examples: vec!["Alice's full name is Alice Smith".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "died_at".into(),
            object_kind: ObjectKind::Literal(LiteralType::DateTime),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Static,
            invalidation: Invalidation::ExplicitOnly,
            symmetric: false,
            inverse_of: None,
            description: "When this person died. A second value is a contradiction.".into(),
            examples: vec!["Alice died at 2024-03-01T00:00:00Z".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "knows".into(),
            object_kind: ObjectKind::Entity("Person".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: true,
            inverse_of: None,
            description: "This person knows another person. Symmetric.".into(),
            examples: vec!["Alice knows Bob".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "member_of".into(),
            object_kind: ObjectKind::Entity("Organization".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "Organizations this person is a member of.".into(),
            examples: vec!["Alice is a member of the Engineering Guild".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "part_of".into(),
            object_kind: ObjectKind::Entity(["Organization", "Project"].into()),
            subject_types: vec!["Organization".into(), "Project".into(), "Artifact".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Static,
            invalidation: Invalidation::Supersede,
            symmetric: false,
            inverse_of: None,
            description:
                "This organization/project/artifact is part of a parent organization or project."
                    .into(),
            examples: vec!["Acme subsidiary is part of Acme Corp".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "located_in".into(),
            object_kind: ObjectKind::Entity("Place".into()),
            subject_types: vec!["Place".into(), "Artifact".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Static,
            invalidation: Invalidation::Supersede,
            symmetric: false,
            inverse_of: None,
            description: "This place or artifact is located within a place.".into(),
            examples: vec!["Seoul is located in South Korea".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "has_skill".into(),
            object_kind: ObjectKind::Entity("Concept".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "A skill or competency this person has.".into(),
            examples: vec!["Alice has skill Rust programming".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "created_by".into(),
            object_kind: ObjectKind::Entity("Person".into()),
            subject_types: vec!["Artifact".into(), "Document".into()],
            cardinality: Cardinality::Functional,
            temporality: Temporality::Static,
            invalidation: Invalidation::Supersede,
            symmetric: false,
            inverse_of: Some("author_of".into()),
            description: "Who created this artifact or document.".into(),
            examples: vec!["The report was created by Alice".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 1.0,
        },
        PredicateDef {
            name: "aliases".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec![
                "Person".into(),
                "Organization".into(),
                "Project".into(),
                "Artifact".into(),
            ],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Static,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "Alternative names for this entity. Multiple values coexist.".into(),
            examples: vec!["Alice's alias is A. Smith".into()],
            deprecated_by: None,
            profile_relevant: true,
            confidence_prior: 1.0,
        },
        // ── Hearsay / uncertain predicates (§5.5, M10 10.9) ──────────────
        // These carry a low confidence_prior so beliefs derived from them
        // are automatically down-weighted by the fold. No pipeline
        // special-casing required (P4: semantics in the registry).
        PredicateDef {
            name: "allegedly_employed_by".into(),
            object_kind: ObjectKind::Entity("Organization".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "Unverified claim that an organization employs this person.".into(),
            examples: vec!["Alice is allegedly employed by Acme Corp".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.3,
        },
        PredicateDef {
            name: "rumored_knows".into(),
            object_kind: ObjectKind::Entity("Person".into()),
            subject_types: vec!["Person".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "Unverified claim that this person knows another person.".into(),
            examples: vec!["Alice is rumored to know Bob".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.3,
        },
        // ── Negative / uncertain family (§5.5, M10 10.9) ─────────────────
        // Agent traces record what did not work and what is not known.
        // Low priors keep beliefs derived from them down-weighted by the
        // fold — no pipeline special-casing (P4).
        PredicateDef {
            name: "failed_because".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec![
                "Task".into(),
                "Project".into(),
                "Code".into(),
                "Artifact".into(),
            ],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Point,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "One observed failure and its cause: an attempt at this task, project, code, or artifact failed for the stated reason. Point-in-time; the generalized recurring form is `fails_when`.".into(),
            examples: vec![
                "The deploy attempt failed because the migration lock was held".into(),
            ],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.6,
        },
        PredicateDef {
            name: "assumed".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec![
                "Task".into(),
                "Concept".into(),
                "Code".into(),
                "Person".into(),
            ],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "An assumption made about this subject, stated without verification. Record it as an assumption, not as true.".into(),
            examples: vec!["The script assumed the database was empty at start".into()],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.4,
        },
        PredicateDef {
            name: "unknown_whether".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec![
                "Task".into(),
                "Concept".into(),
                "Project".into(),
                "Person".into(),
            ],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "An open question about this subject: something explicitly not known or not yet verified.".into(),
            examples: vec![
                "It is unknown whether the cache is shared across workers".into(),
            ],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.4,
        },
        // ── Experience family (doc/spec/experience-memory-v1.md §3, M11) ─
        PredicateDef {
            name: "requires".into(),
            object_kind: ObjectKind::Entity(["Concept", "Artifact", "Code", "Document"].into()),
            subject_types: vec!["Task".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "A prerequisite for performing this task in its environment: a tool, document, artifact, or concept it depends on.".into(),
            examples: vec![
                "Deploying the checkout service requires the migration playbook".into(),
            ],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.7,
        },
        PredicateDef {
            name: "fails_when".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec![
                "Task".into(),
                "Code".into(),
                "Artifact".into(),
                "Project".into(),
            ],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "A recurring condition under which this task, code, artifact, or project fails in this environment; a gotcha. Assert only when the source shows repetition or states it explicitly.".into(),
            examples: vec![
                "The build fails when the vendored crate cache is cold".into(),
            ],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.6,
        },
        PredicateDef {
            name: "premise_differs".into(),
            object_kind: ObjectKind::Literal(LiteralType::Text),
            subject_types: vec!["Task".into(), "Concept".into(), "Code".into()],
            cardinality: Cardinality::MultiValued,
            temporality: Temporality::Interval,
            invalidation: Invalidation::Coexist,
            symmetric: false,
            inverse_of: None,
            description: "An assumption that is valid in other environments but is wrong for this subject here.".into(),
            examples: vec![
                "The docs assume the firewall opens these ports by default; on this host it does not".into(),
            ],
            deprecated_by: None,
            profile_relevant: false,
            confidence_prior: 0.6,
        },
    ]
});

/// The shipped core/v1 ontology (DESIGN §5.5).
pub fn core_v1() -> &'static Vec<PredicateDef> {
    &CORE_V1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn core_v1_covers_all_invalidation_branches() {
        let defs = core_v1();
        // Must have at least one predicate for each branch we need to test:
        let has_func_supersede_interval = defs.iter().any(|d| {
            d.cardinality == Cardinality::Functional
                && d.invalidation == Invalidation::Supersede
                && d.temporality == Temporality::Interval
        });
        let has_func_supersede_static = defs.iter().any(|d| {
            d.cardinality == Cardinality::Functional
                && d.invalidation == Invalidation::Supersede
                && d.temporality == Temporality::Static
        });
        let has_multi_coexist = defs.iter().any(|d| {
            d.cardinality == Cardinality::MultiValued && d.invalidation == Invalidation::Coexist
        });
        let has_explicit_only = defs
            .iter()
            .any(|d| d.invalidation == Invalidation::ExplicitOnly);
        let has_symmetric = defs.iter().any(|d| d.symmetric);
        let has_inverse = defs.iter().any(|d| d.inverse_of.is_some());

        assert!(
            has_func_supersede_interval,
            "missing Functional/Supersede/Interval"
        );
        assert!(
            has_func_supersede_static,
            "missing Functional/Supersede/Static"
        );
        assert!(has_multi_coexist, "missing MultiValued/Coexist");
        assert!(has_explicit_only, "missing ExplicitOnly");
        assert!(has_symmetric, "missing symmetric");
        assert!(has_inverse, "missing inverse_of");
    }

    #[test]
    fn predicate_names_are_unique() {
        let names: HashSet<_> = core_v1().iter().map(|d| &d.name).collect();
        assert_eq!(names.len(), core_v1().len(), "duplicate predicate names");
    }

    #[test]
    fn predicate_defs_serialize_roundtrip() {
        for def in core_v1() {
            let json = serde_json::to_string(def).expect("serialize");
            let back: PredicateDef = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(def.name, back.name);
            assert_eq!(def.cardinality, back.cardinality);
            assert_eq!(def.temporality, back.temporality);
            assert_eq!(def.invalidation, back.invalidation);
        }
    }

    #[test]
    fn part_of_accepts_multiple_object_types() {
        // The regression this redesign exists for: project-part-of-project
        // knowledge must survive validation (minor 4).
        let def = core_v1()
            .iter()
            .find(|d| d.name == "part_of")
            .expect("part_of");
        let ObjectKind::Entity(types) = &def.object_kind else {
            panic!("part_of object must be Entity");
        };
        assert!(types.0.iter().any(|t| t == "Organization"));
        assert!(types.0.iter().any(|t| t == "Project"));
    }

    #[test]
    fn object_kind_entity_roundtrips_through_array() {
        let def = core_v1()
            .iter()
            .find(|d| d.name == "part_of")
            .expect("part_of");
        let json = serde_json::to_string(&def.object_kind).expect("serialize");
        assert_eq!(json, r#"["Organization","Project"]"#);
        let back: ObjectKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, def.object_kind);
    }

    #[test]
    fn object_kind_accepts_legacy_single_string() {
        // def_json rows written by registry minor <= 3 stored Entity as a
        // bare string; they must keep loading (no migration required).
        let k: ObjectKind = serde_json::from_str(r#""Organization""#).expect("legacy string");
        assert_eq!(k, ObjectKind::Entity(["Organization"].into()));
    }

    #[test]
    fn object_kind_literal_strings_stay_literal() {
        // Variant-order guard: Literal is tried before Entity so literal type
        // names are not swallowed by the string-accepting Entity visitor.
        let k: ObjectKind = serde_json::from_str(r#""text""#).expect("literal");
        assert_eq!(k, ObjectKind::Literal(LiteralType::Text));
    }

    #[test]
    fn object_kind_enum_uses_object_shape() {
        // Enum moved from a bare array (ambiguous with multi-type Entity) to
        // an object shape under untagged serde. No core/v1 predicate uses it.
        let k: ObjectKind = serde_json::from_str(r#"{"variants":["yes","no"]}"#).expect("enum");
        assert_eq!(
            k,
            ObjectKind::Enum {
                variants: vec!["yes".into(), "no".into()]
            }
        );
        let json = serde_json::to_string(&k).expect("serialize");
        assert_eq!(json, r#"{"variants":["yes","no"]}"#);
    }
}
