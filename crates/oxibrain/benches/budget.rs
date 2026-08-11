//! §13.2 performance budget benchmarks.

use criterion::{Criterion, criterion_group, criterion_main};
use oxibrain::Brain;
use oxibrain_core::retrieval::{Query, QueryMode, TraversalSpec};
use oxibrain_ports::{FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use std::sync::Arc;

fn build_fixture(dir: &std::path::Path) -> Brain {
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
        let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.to_str().unwrap()), clock)
            .await
            .expect("open");
        let space = brain.ensure_space("bench").await.expect("space");

        // Declare 200 entities, 500 statements in a graph pattern.
        let mut i = 0;
        while i < 200 {
            let subj = format!("Entity{i}");
            let pred = if i % 3 == 0 { "works_on" } else { "knows" };
            let obj = format!("Entity{}", (i + 1) % 200);
            brain
                .declare(
                    &space,
                    Declaration::AddStatement {
                        subject: EntityRef {
                            surface: subj.clone(),
                            ty: "Concept".into(),
                        },
                        predicate: pred.into(),
                        object: DeclObject::Entity {
                            surface: obj.clone(),
                            ty: "Concept".into(),
                        },
                        polarity: "affirm".into(),
                        valid_from: TIME_MIN.millis(),
                        valid_to: TIME_MAX.millis(),
                    },
                )
                .await
                .expect("declare");
            i += 1;
        }
        brain.rebuild_indexes(&space).await.expect("rebuild");
        brain
            .rebuild_communities(&space)
            .await
            .expect("rebuild_communities");
        brain
    })
}

fn bench_declaration_write(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path());
    let space = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(brain.ensure_space("bench"))
        .expect("space");
    c.bench_function("declaration_write", |b| {
        b.iter(|| {
            let brain = &brain;
            let space = &space;
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                brain
                    .declare(
                        space,
                        Declaration::AddStatement {
                            subject: EntityRef {
                                surface: "BenchEntity".into(),
                                ty: "Concept".into(),
                            },
                            predicate: "knows".into(),
                            object: DeclObject::Entity {
                                surface: "BenchTarget".into(),
                                ty: "Concept".into(),
                            },
                            polarity: "affirm".into(),
                            valid_from: TIME_MIN.millis(),
                            valid_to: TIME_MAX.millis(),
                        },
                    )
                    .await
                    .expect("declare");
            });
        });
    });
}

fn bench_hybrid_query(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path());
    let space = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(brain.ensure_space("bench"))
        .expect("space");
    c.bench_function("hybrid_query_top20", |b| {
        b.iter(|| {
            let brain = &brain;
            let space = &space;
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                brain
                    .query(Query {
                        text: "Entity50".into(),
                        mode: QueryMode::Hybrid,
                        space: space.clone(),
                        as_of: None,
                        limit: 20,
                        min_confidence: 0.0,
                    })
                    .await
                    .expect("query");
            });
        });
    });
}

fn bench_traversal(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path());
    let space = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(brain.ensure_space("bench"))
        .expect("space");
    c.bench_function("traversal_depth3_256", |b| {
        b.iter(|| {
            let brain = &brain;
            let space = &space;
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                brain
                    .traverse(
                        space,
                        TraversalSpec {
                            start: vec!["test_entity".into()],
                            ..Default::default()
                        },
                    )
                    .await
                    .ok();
            });
        });
    });
}

criterion_group!(
    benches,
    bench_declaration_write,
    bench_hybrid_query,
    bench_traversal
);
criterion_main!(benches);
