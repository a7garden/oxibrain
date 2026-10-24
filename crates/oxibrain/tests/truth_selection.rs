//! Complete-group selection must be query dependent without changing fold semantics.
use oxibrain::{
    Brain, BrainConfig, DeclObject, Declaration, EntityRef, ReadPolicy, ReadScope, SearchRequest,
    Timestamp,
};
use oxibrain_core::retrieval::{QueryMode, SearchPlane};
use oxibrain_ports::FakeClock;
use std::sync::Arc;
fn fact(person: &str, company: &str) -> Declaration {
    Declaration::AddStatement {
        subject: EntityRef {
            surface: person.into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: company.into(),
            ty: "Organization".into(),
        },
        polarity: "affirm".into(),
        valid_from: 0,
        valid_to: i64::MAX,
    }
}
fn query(space: &str, known: Option<Timestamp>) -> SearchRequest {
    SearchRequest {
        text: "QueryTargetPerson".into(),
        scope: ReadScope {
            space: space.into(),
            known_at: known,
            as_of: None,
            min_confidence: 0.0,
        },
        planes: [SearchPlane::Memory].into_iter().collect(),
        mode: QueryMode::Lexical,
        limit: 10,
    }
}
#[tokio::test]
async fn irrelevant_groups_do_not_exhaust_the_requested_complete_group() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(100)));
    let brain = Brain::with_clock(BrainConfig::at(dir.path()), clock.clone())
        .await
        .unwrap()
        .with_read_policy(ReadPolicy {
            truth_bytes: 1100,
            ..Default::default()
        })
        .unwrap();
    let space = brain.ensure_space("fixture").await.unwrap();
    brain
        .declare(&space, fact("QueryTargetPerson", "FirstCompany"))
        .await
        .unwrap();
    for i in 0..40 {
        brain
            .declare(
                &space,
                fact(
                    &format!(
                        "Person{}",
                        blake3::hash(format!("person-{i}").as_bytes()).to_hex()
                    ),
                    &format!(
                        "Company{}",
                        blake3::hash(format!("company-{i}").as_bytes()).to_hex()
                    ),
                ),
            )
            .await
            .unwrap();
    }
    let first = brain.search_with_scope(query(&space, None)).await.unwrap();
    assert!(
        serde_json::to_string(&first.data)
            .unwrap()
            .contains("FirstCompany")
    );
    let receipt = first.meta.truth_selection.unwrap();
    assert_eq!(receipt.examined, 41);
    assert!(receipt.sources >= receipt.materialized);
    assert!(receipt.materialized < 41 && receipt.bytes <= 1100);
    clock.advance(100);
    let mut replacement = fact("QueryTargetPerson", "LaterCompany");
    if let Declaration::AddStatement { valid_from, .. } = &mut replacement {
        *valid_from = 200;
    }
    brain.declare(&space, replacement).await.unwrap();
    let historical = brain
        .search_with_scope(query(&space, Some(Timestamp(150))))
        .await
        .unwrap();
    let text = serde_json::to_string(&historical.data).unwrap();
    assert!(text.contains("FirstCompany") && !text.contains("LaterCompany"));
    let current = brain.search_with_scope(query(&space, None)).await.unwrap();
    let text = serde_json::to_string(&current.data).unwrap();
    assert!(
        text.contains("LaterCompany") && !text.contains("FirstCompany"),
        "{text}"
    );
    // Neither a current winner nor a matching assertion may bypass completeness.
    let tiny = brain
        .clone()
        .with_read_policy(ReadPolicy {
            truth_bytes: 500,
            ..Default::default()
        })
        .unwrap();
    let missing = tiny.search_with_scope(query(&space, None)).await.unwrap();
    let text = serde_json::to_string(&missing.data).unwrap();
    assert!(!text.contains("LaterCompany") && !text.contains("FirstCompany"));
    assert!(missing.meta.truth_selection.unwrap().omitted > 0);
}

#[tokio::test]
#[ignore = "explicit scale measurement; run with OXIBRAIN_SCALE_OUTPUT"]
async fn measure_truth_selection_scale() {
    let output = std::env::var("OXIBRAIN_SCALE_OUTPUT").unwrap();
    let mut observations = Vec::new();
    for groups in [10, 100, 1000] {
        let dir = tempfile::tempdir().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path()))
            .await
            .unwrap()
            .with_read_policy(ReadPolicy {
                truth_bytes: 1100,
                ..Default::default()
            })
            .unwrap();
        let space = brain.ensure_space("scale").await.unwrap();
        brain
            .declare(&space, fact("QueryTargetPerson", "TargetCompany"))
            .await
            .unwrap();
        for i in 1..groups {
            brain
                .declare(
                    &space,
                    fact(
                        &format!(
                            "Person{}",
                            blake3::hash(format!("person-{i}").as_bytes()).to_hex()
                        ),
                        &format!(
                            "Company{}",
                            blake3::hash(format!("company-{i}").as_bytes()).to_hex()
                        ),
                    ),
                )
                .await
                .unwrap();
        }
        let mut times = Vec::new();
        let mut receipt = None;
        for _ in 0..5 {
            let started = std::time::Instant::now();
            let result = brain.search_with_scope(query(&space, None)).await.unwrap();
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            assert!(
                serde_json::to_string(&result.data)
                    .unwrap()
                    .contains("TargetCompany")
            );
            receipt = result.meta.truth_selection;
        }
        observations
            .push(serde_json::json!({"groups":groups,"query_ms":times,"selection":receipt}));
    }
    std::fs::write(output, serde_json::to_vec_pretty(&observations).unwrap()).unwrap();
}
