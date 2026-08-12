//! `oxibrain predicate list` — print the core/v1 predicate registry (DESIGN §5.5, P4).
//!
//! No store access needed: the registry is the in-process core ontology.

use oxibrain_core::registry::{CORE_V1_MAJOR, CORE_V1_MINOR, LiteralType, ObjectKind, core_v1};

pub fn run() -> anyhow::Result<()> {
    let preds = core_v1();
    println!(
        "core/v1 registry — {} predicates (major={}, minor={})",
        preds.len(),
        CORE_V1_MAJOR,
        CORE_V1_MINOR
    );
    for p in preds {
        println!("  {}", p.name);
        println!(
            "    object={} | cardinality={} | temporality={} | invalidation={} | symmetric={}",
            format_object_kind(&p.object_kind),
            p.cardinality.as_db(),
            p.temporality.as_db(),
            p.invalidation.as_db(),
            p.symmetric,
        );
        if !p.subject_types.is_empty() {
            println!("    subjects: {}", p.subject_types.join(", "));
        }
        if let Some(inv) = &p.inverse_of {
            println!("    inverse_of: {inv}");
        }
        if !p.description.is_empty() {
            println!("    {}", p.description);
        }
    }
    Ok(())
}

fn format_object_kind(k: &ObjectKind) -> String {
    match k {
        ObjectKind::Entity(t) => format!("entity:{t}"),
        ObjectKind::Literal(LiteralType::Text) => "literal:text".into(),
        ObjectKind::Literal(LiteralType::Date) => "literal:date".into(),
        ObjectKind::Literal(LiteralType::DateTime) => "literal:datetime".into(),
        ObjectKind::Literal(LiteralType::Number) => "literal:number".into(),
        ObjectKind::Literal(LiteralType::Bool) => "literal:bool".into(),
        ObjectKind::Literal(LiteralType::Quantity { unit }) => format!("literal:quantity[{unit}]"),
        ObjectKind::Enum(vals) => format!("enum:{{{}}}", vals.join("|")),
    }
}
