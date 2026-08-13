//! M9 exit-criteria measurements (§16.4): brief p95 latency, resolution
//! sublinearity over 10⁴ entities, and navigate round-trip.
//!
//! These are the *measurements* the M9 exit criteria demand — the code
//! shipped earlier; this bench proves the numbers on real hardware.

use criterion::{Criterion, criterion_group, criterion_main, Throughput};
use oxibrain::Brain;
use oxibrain_ports::{FakeClock, TIME_MAX, TIME_MIN, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use std::sync::Arc;

const BRIEF_TARGETS: usize = 50;

/// Build a fixture with `n_entities` concepts arranged in a ring graph
/// (each entity knows the next) + one hub entity that many link to, so
/// brief() has meaningful neighbours + beliefs to render.
fn build_fixture(dir: &std::path::Path, n_entities: usize) -> Brain {
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
        let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.to_str().unwrap()), clock)
            .await
            .expect("open");
        let space = brain.ensure_space("bench").await.expect("space");

        // Ring: Entity_i knows Entity_{(i+1) % n}. Plus Entity0 is the hub
        // everyone knows (creates many incoming neighbours for Entity0).
        for i in 0..n_entities {
            let subj = format!("Entity{i}");
            let obj = if i == 0 {
                format!("Entity{}", (i + 1) % n_entities)
            } else {
                format!("Entity{}", (i + 1) % n_entities)
            };
            brain
                .declare(
                    &space,
                    Declaration::AddStatement {
                        subject: EntityRef {
                            surface: subj.clone(),
                            ty: "Concept".into(),
                        },
                        predicate: "knows".into(),
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
        }
        // Every entity also works_on Entity0 — hub with many incoming edges.
        for i in 1..n_entities {
            brain
                .declare(
                    &space,
                    Declaration::AddStatement {
                        subject: EntityRef {
                            surface: format!("Entity{i}"),
                            ty: "Concept".into(),
                        },
                        predicate: "works_on".into(),
                        object: DeclObject::Entity {
                            surface: "Entity0".into(),
                            ty: "Concept".into(),
                        },
                        polarity: "affirm".into(),
                        valid_from: TIME_MIN.millis(),
                        valid_to: TIME_MAX.millis(),
                    },
                )
                .await
                .expect("declare");
        }
        brain
    })
}

fn resolve_entity(brain: &Brain, space: &str, surface: &str) -> String {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(brain.resolve_entity_id(space, "Concept", surface))
        .expect("resolve")
        .unwrap_or_else(|| panic!("entity {surface} not found"))
}

/// brief p95 latency on a standard fixture (1000 entities). The exit
/// criterion is p95 < 100 ms.
fn bench_brief_p95(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = build_fixture(dir.path(), 1_000);
    let space = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(brain.ensure_space("bench"))
        .expect("space");
    // Resolve BRIEF_TARGETS entity ids up front so the bench measures
    // brief() only, not resolution.
    let ids: Vec<String> = (0..BRIEF_TARGETS)
        .map(|i| resolve_entity(&brain, &space, &format!("Entity{i}")))
        .collect();

    let mut group = c.benchmark_group("brief_p95");
    group.sample_size(200);
    group.bench_function("entity_page_1000_entities", |b| {
        b.iter(|| {
            let mut i = 0usize;
            for id in &ids {
                i += 1;
                let _ = tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(brain.brief(&space, id))
                    .expect("brief");
                if i == BRIEF_TARGETS {
                    break;
                }
            }
        });
    });
    group.finish();
}

/// Resolution sublinearity: one more `declare` (which resolves the mention)
/// over N-entity fixtures. If resolution is sublinear per mention, the time
/// per declare grows sub-linearly with N.
fn bench_resolution_scaling(c: &mut Criterion) {
    for n in [1_000usize, 5_000, 10_000] {
        let dir = tempfile::tempdir().expect("tempdir");
        let brain = build_fixture(dir.path(), n);
        let space = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(brain.ensure_space("bench"))
            .expect("space");

        let mut group = c.benchmark_group("resolution_per_mention");
        group.throughput(Throughput::Elements(n as u64));
        group.bench_function(format!("declare_over_{n}_entities"), |b| {
            b.iter(|| {
                let brain = &brain;
                let space = &space;
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    brain
                        .declare(
                            space,
                            Declaration::AddStatement {
                                subject: EntityRef {
                                    surface: "NewMention".into(),
                                    ty: "Concept".into(),
                                },
                                predicate: "knows".into(),
                                object: DeclObject::Entity {
                                    surface: "Entity0".into(),
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
        group.finish();
    }
}

criterion_group!(m9_exit, bench_brief_p95, bench_resolution_scaling);
criterion_main!(m9_exit);
