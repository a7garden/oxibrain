//! Integration test for oxibrain-embed-local against a real embedding model.
//!
//! Ignored by default — requires a model at `~/.oxi/models/`. Run with:
//!   cargo test -p oxibrain-embed-local --test local_embedder -- --ignored
//! Validated against BGE-M3-Q4_K_M (multilingual, 1024 dims).

use oxibrain_embed_local::{LocalEmbedder, LocalEmbedderOptions};
use oxibrain_ports::EmbeddingPort;
use std::path::PathBuf;

fn model_path() -> PathBuf {
    let mut p = home_dir();
    p.push(".oxi/models/bge-m3-Q4_K_M.gguf");
    p
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[test]
#[ignore = "requires GGUF model download"]
fn embedder_loads_and_embeds() {
    let path = model_path();
    assert!(path.exists(), "model not found at {path:?}");
    let emb = LocalEmbedder::open(&path, LocalEmbedderOptions::default()).expect("open");
    assert_eq!(emb.dim(), 1024, "BGE-M3 embedding dim");

    // Multilingual: same sentence in 3 scripts should give similar vectors.
    let en = "The capital of France is Paris";
    let ko = "프랑스의 수도는 파리이다";
    let ja = "フランスの首都はパリです";

    let vecs = emb.embed(&[en, ko, ja]).expect("embed");
    assert_eq!(vecs.len(), 3);
    for v in &vecs {
        assert_eq!(v.len(), emb.dim());
        // L2-normalized: unit length.
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "expected unit norm, got {norm}");
    }

    // Cross-lingual similarity must be high (same meaning, different scripts).
    let sim_en_ko = cosine(&vecs[0], &vecs[1]);
    let sim_en_ja = cosine(&vecs[0], &vecs[2]);
    assert!(sim_en_ko > 0.5, "EN-KO similarity too low: {sim_en_ko:.3}");
    assert!(sim_en_ja > 0.5, "EN-JA similarity too low: {sim_en_ja:.3}");
    eprintln!("similarities: EN-KO {sim_en_ko:.3}, EN-JA {sim_en_ja:.3}");

    // Unrelated text must be less similar.
    let unrelated = "Quantum entanglement is a physical phenomenon";
    let vecs2 = emb.embed(&[unrelated]).expect("embed unrelated");
    let sim_unrelated = cosine(&vecs[0], &vecs2[0]);
    assert!(
        sim_unrelated < sim_en_ko,
        "unrelated {sim_unrelated:.3} should be less similar than same-meaning EN-KO {sim_en_ko:.3}"
    );
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
}
