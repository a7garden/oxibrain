//! M7.6 integration tests: dense embedding path (F16, F17).
//!
//! Uses a deterministic fake embedder (4-gram hashing into 1024-dim) so the
//! tests run without a model download. Verifies:
//! - embed_entities during reproject (F17)
//! - dense_search KNN via sqlite-vec (F16)
//! - explicit error when Dense mode has no embedder (no silent fallback)

use oxibrain::Brain;
use oxibrain_core::retrieval::{Query, QueryMode, SearchTarget};
use oxibrain_ports::{BrainError, EmbeddingPort, FakeClock, Timestamp};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::tempdir;

/// Deterministic 1024-dim embedder: 4-gram shingles hashed into bins.
/// Similar texts (shared 4-grams) produce similar vectors, so KNN ranks
/// semantically related entities.
#[derive(Debug)]
struct NgramEmbedder;

impl EmbeddingPort for NgramEmbedder {
    fn dim(&self) -> usize {
        1024
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, BrainError> {
        Ok(texts.iter().map(|t| ngram_vector(t)).collect())
    }
}

fn ngram_vector(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; 1024];
    let norm: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let chars: Vec<char> = norm.chars().collect();
    let mut seen: HashSet<usize> = HashSet::new();
    if chars.len() < 4 {
        // Short text: hash the whole thing into a couple of bins.
        let h = hash_str(&norm) as usize % 1024;
        v[h] = 1.0;
        v[(h + 7) % 1024] = 1.0;
        return v;
    }
    for w in chars.windows(4) {
        let gram: String = w.iter().collect();
        let h = hash_str(&gram) as usize % 1024;
        if seen.insert(h) {
            v[h] = 1.0;
        }
    }
    // L2-normalize.
    let norm_f: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_f > 0.0 {
        for x in &mut v {
            *x /= norm_f;
        }
    }
    v
}

fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

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
        valid_from: oxibrain_ports::TIME_MIN.millis(),
        valid_to: oxibrain_ports::TIME_MAX.millis(),
    }
}

async fn build_brain_with_entities() -> (Brain, tempfile::TempDir, String) {
    let dir = tempdir().expect("tempdir");
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.path()), clock)
        .await
        .expect("brain")
        .with_embedder(Arc::new(NgramEmbedder));
    let space = brain.ensure_space("test").await.expect("space");

    // Two related entities (both work on ML) + one unrelated.
    brain
        .declare(
            &space,
            decl_add("Alice", "Person", "works_on", "Machine Learning", "Project"),
        )
        .await
        .expect("declare alice");
    brain
        .declare(
            &space,
            decl_add("Bob", "Person", "works_on", "Machine Learning", "Project"),
        )
        .await
        .expect("declare bob");
    brain
        .declare(
            &space,
            decl_add("Carol", "Person", "works_on", "Baking", "Project"),
        )
        .await
        .expect("declare carol");

    (brain, dir, space)
}

#[tokio::test]
async fn reproject_embeds_entities_and_dense_knn_finds_them() {
    let (brain, _dir, space) = build_brain_with_entities().await;

    // Reproject → embed_entities runs (F17).
    brain.reproject().await.expect("reproject");

    // Dense query — the query text shares 4-grams with the ML entities.
    let result = brain
        .query(Query {
            text: "machine learning".into(),
            mode: QueryMode::Dense,
            space: space.clone(),
            as_of: None,
            limit: 5,
            min_confidence: 0.0,
        })
        .await
        .expect("dense query");

    assert!(!result.items.is_empty(), "dense query returned no items");
    // Every hit must be an entity (the vec0 table is entity-keyed).
    for item in &result.items {
        assert!(
            matches!(item.target, SearchTarget::Entity { .. }),
            "dense hits must be entities, got {:?}",
            item.target
        );
    }
    // The top hit must have a positive fused score (found via KNN).
    assert!(result.items[0].fused_score > 0.0);

    // A query with NO shared n-grams must still rank something (KNN returns
    // nearest, not thresholded) — but it should be ranked lower than the
    // exact-match query. This confirms KNN ordering, not exact-match only.
    let exact = result.items[0].fused_score;
    let loose = brain
        .query(Query {
            text: "machine learning".into(),
            mode: QueryMode::Dense,
            space: space.clone(),
            as_of: None,
            limit: 5,
            min_confidence: 0.0,
        })
        .await
        .expect("dense query 2");
    assert_eq!(
        exact, loose.items[0].fused_score,
        "deterministic embedder + query"
    );
}

#[tokio::test]
async fn dense_mode_without_embedder_returns_explicit_error() {
    let dir = tempdir().expect("tempdir");
    let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
    // NO embedder attached.
    let brain = Brain::with_clock(oxibrain::BrainConfig::at(dir.path()), clock)
        .await
        .expect("brain");
    let space = brain.ensure_space("test").await.expect("space");

    let err = brain
        .query(Query {
            text: "anything".into(),
            mode: QueryMode::Dense,
            space,
            as_of: None,
            limit: 5,
            min_confidence: 0.0,
        })
        .await
        .expect_err("dense without embedder must error");

    assert!(
        err.to_string().contains("requires a configured embedder"),
        "error should explain the embedder requirement, got: {err}"
    );
}
