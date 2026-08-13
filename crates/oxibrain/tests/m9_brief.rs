//! M9 §9.2/§9.6: `brief(entity)` renders a page with identity, beliefs,
//! neighbours (followable links), timeline and sources — and is deterministic.

use oxibrain::Brain;
use oxibrain_ports::{FakeClock, Timestamp, TIME_MAX, TIME_MIN};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use std::sync::Arc;
use tempfile::tempdir;

fn decl_add(subj: &str, subj_ty: &str, pred: &str, obj: &str, obj_ty: &str) -> Declaration {
    Declaration::AddStatement {
        subject: EntityRef {
            surface: subj.into(),
            ty: subj_ty.into(),
        },
        predicate: pred.into(),
        object: DeclObject::Entity {
            surface: obj.into(),
            ty: obj_ty.into(),
        },
        polarity: "affirm".into(),
        valid_from: TIME_MIN.millis(),
        valid_to: TIME_MAX.millis(),
    }
}

#[tokio::test]
async fn brief_renders_and_is_deterministic() {
    let dir = tempdir().expect("tempdir");
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.path()), clock)
        .await
        .expect("brain");
    let space = brain.ensure_space("test").await.expect("space");
    for decl in [
        decl_add("Alice", "Person", "works_on", "Project X", "Project"),
        decl_add("Alice", "Person", "born_in", "Seoul", "Place"),
        decl_add("Alice", "Person", "knows", "Bob", "Person"),
    ] {
        brain.declare(&space, decl).await.expect("declare");
    }

    let alice = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("Alice exists");

    let brief = brain.brief(&space, &alice).await.expect("brief");

    // Identity + beliefs + neighbours with a followable link.
    assert!(brief.contains("Alice"), "brief must title the entity:\n{brief}");
    assert!(brief.contains("Person"), "brief must name the type:\n{brief}");
    assert!(brief.contains("works_on"), "brief must list beliefs:\n{brief}");
    assert!(brief.contains("Project X"), "brief must render the object:\n{brief}");
    assert!(
        brief.contains("entity://"),
        "brief must contain followable links:\n{brief}"
    );
    assert!(brief.contains("Seoul"), "brief must include all beliefs:\n{brief}");

    // Determinism: brief twice on an unchanged ledger is byte-equal (§14.2).
    let brief2 = brain.brief(&space, &alice).await.expect("brief2");
    assert_eq!(brief, brief2, "brief must be deterministic");

    // Navigate follows a link to Bob (an incoming neighbour of Alice).
    let bob = brain
        .resolve_entity_id(&space, "Person", "Bob")
        .await
        .expect("resolve bob")
        .expect("Bob exists");
    let bob_page = brain
        .navigate(&space, "ignored", &format!("entity://{bob}"))
        .await
        .expect("navigate");
    assert!(bob_page.contains("Bob"), "navigate must render the target:\n{bob_page}");
}
