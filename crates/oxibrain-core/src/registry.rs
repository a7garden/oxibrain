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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ObjectKind {
    Entity(EntityTypeRef),
    Literal(LiteralType),
    Enum(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub object_kind: ObjectKind,
    pub subject_types: Vec<EntityTypeRef>,
    pub cardinality: Cardinality,
    pub temporality: Temporality,
    pub invalidation: Invalidation,
    pub symmetric: bool,
    pub inverse_of: Option<PredicateRef>,
    pub description: String,
    pub examples: Vec<String>,
    pub deprecated_by: Option<PredicateRef>,
}

/// Registry version for this ontology.
pub const CORE_V1_MAJOR: u32 = 1;
pub const CORE_V1_MINOR: u32 = 0;

static CORE_V1: std::sync::LazyLock<Vec<PredicateDef>> = std::sync::LazyLock::new(|| vec![
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
    },
    PredicateDef {
        name: "part_of".into(),
        object_kind: ObjectKind::Entity("Organization".into()),
        subject_types: vec!["Organization".into()],
        cardinality: Cardinality::Functional,
        temporality: Temporality::Static,
        invalidation: Invalidation::Supersede,
        symmetric: false,
        inverse_of: None,
        description: "This organization is part of a parent organization.".into(),
        examples: vec!["Acme subsidiary is part of Acme Corp".into()],
        deprecated_by: None,
    },
    PredicateDef {
        name: "located_in".into(),
        object_kind: ObjectKind::Entity("Place".into()),
        subject_types: vec!["Place".into()],
        cardinality: Cardinality::Functional,
        temporality: Temporality::Static,
        invalidation: Invalidation::Supersede,
        symmetric: false,
        inverse_of: None,
        description: "This place is located within another place.".into(),
        examples: vec!["Seoul is located in South Korea".into()],
        deprecated_by: None,
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
    },
    PredicateDef {
        name: "aliases".into(),
        object_kind: ObjectKind::Literal(LiteralType::Text),
        subject_types: vec!["Person".into()],
        cardinality: Cardinality::MultiValued,
        temporality: Temporality::Static,
        invalidation: Invalidation::Coexist,
        symmetric: false,
        inverse_of: None,
        description: "Alternative names for this person. Multiple values coexist.".into(),
        examples: vec!["Alice's alias is A. Smith".into()],
        deprecated_by: None,
    },
]);

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

        assert!(has_func_supersede_interval, "missing Functional/Supersede/Interval");
        assert!(has_func_supersede_static, "missing Functional/Supersede/Static");
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
}
