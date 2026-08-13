//! §5.1 ranking-tolerance calibration (ARCHITECTURE.md §5.1, §17.4).
//!
//! Measures the cross-backend variance of the shipped quantized encoder
//! (BGE-M3) between CPU and Metal, as recall@10 on the fixed probe set in
//! `eval/probes/probes.toml`. The ranking-half tolerance is
//! `max(2pp, 2 × observed_max_delta)`.
//!
//! Ignored by default — requires the model at `~/.oxi/models/bge-m3-Q4_K_M.gguf`
//! and, for the Metal arm, an Apple GPU. Run with:
//!
//!   cargo test -p oxibrain-embed-local --test ranking_equivalence -- --ignored --nocapture
//!
//! The test asserts the two backends agree within the recorded tolerance and
//! prints the measured deltas so a recalibration can be recorded in §5.1.

use oxibrain_embed_local::{LocalEmbedder, LocalEmbedderOptions};
use oxibrain_ports::EmbeddingPort;
use serde::Deserialize;
use std::path::PathBuf;

/// The recorded ranking-half tolerance (percentage points, as a fraction).
///
/// Set from the measurement: `max(0.02, 2 × observed_max_delta)`. Update this
/// and ARCHITECTURE.md §5.1 together whenever the probe set or encoder changes.
const RECORDED_TOLERANCE: f64 = 0.02;

#[derive(Debug, Deserialize)]
struct ProbeSet {
    entities: Vec<Entity>,
    queries: Vec<Query>,
}

#[derive(Debug, Deserialize)]
struct Entity {
    id: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct Query {
    text: String,
    relevant: Vec<String>,
}

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

fn probes_path() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../eval/probes/probes.toml"))
}

fn load_probes() -> ProbeSet {
    let path = probes_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read probe set {}: {e}", path.display()));
    toml::from_str(&raw).expect("parse probe set")
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
}

/// Recall@10 for one query against all embedded entities, by cosine ranking.
fn recall_at_10(query_vec: &[f32], entity_vecs: &[(String, Vec<f32>)], relevant: &[String]) -> f64 {
    let mut scored: Vec<(f32, &str)> = entity_vecs
        .iter()
        .map(|(id, v)| (cosine(query_vec, v), id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let top: std::collections::HashSet<&str> = scored
        .iter()
        .take(10)
        .map(|(_, id)| *id)
        .collect();
    let found = relevant
        .iter()
        .filter(|r| top.contains(r.as_str()))
        .count();
    if relevant.is_empty() {
        1.0
    } else {
        found as f64 / relevant.len() as f64
    }
}

/// One recall@10 pass: embed entities + queries once, average recall over queries.
fn run_pass(embedder: &dyn EmbeddingPort, probes: &ProbeSet) -> f64 {
    let entity_texts: Vec<&str> = probes.entities.iter().map(|e| e.text.as_str()).collect();
    let entity_embeds = embedder.embed(&entity_texts).expect("embed entities");
    let entity_vecs: Vec<(String, Vec<f32>)> = probes
        .entities
        .iter()
        .zip(entity_embeds.iter())
        .map(|(e, v)| (e.id.clone(), v.clone()))
        .collect();

    let query_texts: Vec<&str> = probes.queries.iter().map(|q| q.text.as_str()).collect();
    let query_embeds = embedder.embed(&query_texts).expect("embed queries");

    let recalls: Vec<f64> = probes
        .queries
        .iter()
        .zip(query_embeds.iter())
        .map(|(q, qv)| recall_at_10(qv, &entity_vecs, &q.relevant))
        .collect();
    recalls.iter().sum::<f64>() / recalls.len() as f64
}

fn measure(embedder: &dyn EmbeddingPort, probes: &ProbeSet, runs: usize) -> Vec<f64> {
    (0..runs).map(|_| run_pass(embedder, probes)).collect()
}

#[test]
#[ignore = "requires GGUF model download; CPU vs Metal cross-backend measurement"]
fn ranking_equivalence_cpu_vs_metal() {
    let path = model_path();
    assert!(path.exists(), "model not found at {path:?}");
    let probes = load_probes();
    assert!(!probes.entities.is_empty() && !probes.queries.is_empty());

    const RUNS: usize = 10;

    let cpu = LocalEmbedder::open(
        &path,
        LocalEmbedderOptions {
            n_gpu_layers: 0, // CPU only
            ..LocalEmbedderOptions::default()
        },
    )
    .expect("open CPU embedder");
    let metal = LocalEmbedder::open(&path, LocalEmbedderOptions::default()).expect("open Metal embedder");

    let cpu_recalls = measure(&cpu, &probes, RUNS);
    let metal_recalls = measure(&metal, &probes, RUNS);

    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let cpu_mean = mean(&cpu_recalls);
    let metal_mean = mean(&metal_recalls);

    // Observed max delta: worst-case |cpu_i − metal_j| over all run pairs.
    let mut observed_max_delta: f64 = 0.0;
    for &c in &cpu_recalls {
        for &m in &metal_recalls {
            let d = (c - m).abs();
            if d > observed_max_delta {
                observed_max_delta = d;
            }
        }
    }

    eprintln!("ranking_equivalence (recall@10, {RUNS} runs each):");
    eprintln!("  cpu   mean = {cpu_mean:.4}");
    eprintln!("  metal mean = {metal_mean:.4}");
    eprintln!("  observed_max_delta = {observed_max_delta:.4} ({:.2}pp)", observed_max_delta * 100.0);
    eprintln!(
        "  tolerance = max(2pp, 2×delta) = {:.2}pp",
        (observed_max_delta * 2.0).max(0.02) * 100.0
    );

    assert!(
        observed_max_delta <= RECORDED_TOLERANCE + 1e-9,
        "cross-backend recall@10 delta {observed_max_delta:.4} exceeds recorded tolerance {RECORDED_TOLERANCE:.4}"
    );
}
