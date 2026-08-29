//! `admin predicate list` — the predicate registry (DESIGN §5.5, P4).
//!
//! Store-aware: lists the predicates persisted in the brain store (the
//! core/v1 seed plus every custom registration), marking each row's origin.
//! When the store cannot be opened, falls back to the in-process core/v1
//! ontology so the command stays useful before `admin init`.

use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_core::registry::{
    CORE_V1_MAJOR, CORE_V1_MINOR, LiteralType, ObjectKind, PredicateDef, core_v1,
};
use oxibrain_store::project::Declaration;
use std::fmt::Write as _;
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let opened = match Brain::open_ro(BrainConfig::read_only_at(dir)).await {
        Ok(brain) => match brain.list_predicates().await {
            Ok(defs) => Some(defs),
            Err(e) => {
                eprintln!("note: registry read failed ({e}); showing in-process core/v1");
                None
            }
        },
        Err(e) => {
            eprintln!(
                "note: store unavailable at {} ({e}); showing in-process core/v1",
                dir.display()
            );
            None
        }
    };
    match opened {
        Some(defs) => print!("{}", render(dir, &defs, true)),
        None => print!("{}", render(dir, core_v1(), false)),
    }
    Ok(())
}

/// Render the registry listing. Store mode marks each row core vs custom
/// (membership in the shipped core/v1 ontology); the in-process fallback can
/// only show core/v1.
fn render(dir: &Path, defs: &[PredicateDef], from_store: bool) -> String {
    let is_core = |p: &PredicateDef| core_v1().iter().any(|c| c.name == p.name);
    let mut out = String::new();
    if from_store {
        let core = defs.iter().filter(|p| is_core(p)).count();
        let _ = writeln!(
            out,
            "predicate registry — {} predicates ({} core/v1, {} custom) — {}",
            defs.len(),
            core,
            defs.len() - core,
            dir.display()
        );
    } else {
        let _ = writeln!(
            out,
            "core/v1 registry — {} predicates (major={}, minor={}) — {}",
            defs.len(),
            CORE_V1_MAJOR,
            CORE_V1_MINOR,
            dir.display()
        );
    }
    for p in defs {
        let origin = if is_core(p) { "[core/v1]" } else { "[custom]" };
        let _ = writeln!(out, "  {} {}", p.name, origin);
        let _ = writeln!(
            out,
            "    object={} | cardinality={} | temporality={} | invalidation={} | symmetric={}",
            format_object_kind(&p.object_kind),
            p.cardinality.as_db(),
            p.temporality.as_db(),
            p.invalidation.as_db(),
            p.symmetric,
        );
        if !p.subject_types.is_empty() {
            let _ = writeln!(out, "    subjects: {}", p.subject_types.join(", "));
        }
        if let Some(inv) = &p.inverse_of {
            let _ = writeln!(out, "    inverse_of: {inv}");
        }
        if !p.description.is_empty() {
            let _ = writeln!(out, "    {}", p.description);
        }
    }
    out
}

pub async fn run_add(dir: &Path, json: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = crate::cmd::space_id(&brain, space).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_mode_marks_core_and_custom() {
        let mut defs = core_v1().clone();
        let mut custom = core_v1()[0].clone();
        custom.name = "custom_likes".into();
        custom.description = "A user-registered predicate.".into();
        defs.push(custom);
        let out = render(Path::new("/b"), &defs, true);
        assert!(
            out.contains("15 predicates (14 core/v1, 1 custom)"),
            "{out}"
        );
        assert!(out.contains("custom_likes [custom]"), "{out}");
        assert!(out.contains("employed_by [core/v1]"), "{out}");
    }

    #[test]
    fn fallback_mode_shows_core_only() {
        let out = render(Path::new("/b"), core_v1(), false);
        assert!(out.starts_with("core/v1 registry"), "{out}");
        assert!(!out.contains("[custom]"), "{out}");
    }
}
