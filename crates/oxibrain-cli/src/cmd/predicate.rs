//! `oxibrain predicate list` — print the core/v1 predicate registry (DESIGN §5.5, P4).
//!
//! No store access needed: the registry is the in-process core ontology.

use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::registry::{CORE_V1_MAJOR, CORE_V1_MINOR, LiteralType, ObjectKind, core_v1};
use oxibrain_store::project::Declaration;
use std::path::Path;

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

pub async fn run_add(dir: &Path, json: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    // Parse to extract name for the declaration.
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse predicate def: {e}"))?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("predicate def must have 'name' field"))?
        .to_string();
    let decl = Declaration::RegisterPredicate {
        name,
        def_json: json.to_string(),
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("predicate registered as episode: {ep_id}");
    Ok(())
}

fn format_object_kind(k: &ObjectKind) -> String {
    match k {
        ObjectKind::Entity(types) => format!("entity:{{{}}}", types.0.join("|")),
        ObjectKind::Literal(LiteralType::Text) => "literal:text".into(),
        ObjectKind::Literal(LiteralType::Date) => "literal:date".into(),
        ObjectKind::Literal(LiteralType::DateTime) => "literal:datetime".into(),
        ObjectKind::Literal(LiteralType::Number) => "literal:number".into(),
        ObjectKind::Literal(LiteralType::Bool) => "literal:bool".into(),
        ObjectKind::Literal(LiteralType::Quantity { unit }) => format!("literal:quantity[{unit}]"),
        ObjectKind::Enum { variants: vals } => format!("enum:{{{}}}", vals.join("|")),
    }
}
