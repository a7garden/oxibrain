//! Lexical-vector model: character n-gram shingles with hashing trick
//! (deterministic, fixed dimensionality).
//!
//! Replaces the v1.0 English word tokenizer (F23: stopword list, F24: byte-length
//! filter) with language-independent character n-grams from `ngram::shingles`.
//! The hashing trick and fixed dimensionality are preserved — determinism is
//! unchanged, only the feature space becomes script-neutral (P11).

use crate::ngram;

fn fnv1a(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Extract character n-gram features from text.
///
/// Returns 3-gram shingles of the lowercased text, including boundary
/// sentinels. Language-independent (P11): no word boundaries, no stopword list,
/// no byte-length filter, no script check.
pub fn features(text: &str) -> Vec<String> {
    let normalized = text.to_lowercase();
    ngram::shingles(&normalized, 3).into_iter().collect()
}

pub struct TfIdfModel {
    pub dim: usize,
    idf: Vec<f32>,
    pub n_docs: usize,
}
impl TfIdfModel {
    pub fn fit(texts: &[&str], dim: usize) -> Self {
        let n_docs = texts.len();
        let mut df = vec![0u32; dim];
        for text in texts {
            let mut seen = std::collections::HashSet::new();
            for t in features(text) {
                seen.insert((fnv1a(&t) as usize) % dim);
            }
            for d in seen {
                df[d] += 1;
            }
        }
        let idf = df
            .iter()
            .map(|&d| ((1.0 + n_docs as f32) / (1.0 + d as f32)).ln() + 1.0)
            .collect();
        Self { dim, idf, n_docs }
    }
    pub fn transform(&self, text: &str) -> TfIdfVector {
        let mut v = vec![0f32; self.dim];
        for t in features(text) {
            v[(fnv1a(&t) as usize) % self.dim] += 1.0;
        }
        let mut n = 0.;
        for (i, x) in v.iter_mut().enumerate() {
            *x *= self.idf[i];
            n += *x * *x;
        }
        n = n.sqrt().max(1e-12);
        for x in &mut v {
            *x /= n;
        }
        TfIdfVector(v)
    }
}
pub struct TfIdfVector(Vec<f32>);
impl TfIdfVector {
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }
    pub fn from_vec(v: Vec<f32>) -> Self {
        Self(v)
    }
    pub fn dim(&self) -> usize {
        self.0.len()
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    pub fn from_bytes(b: &[u8]) -> Self {
        Self(
            b.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )
    }
}
pub fn cosine_sim(a: &TfIdfVector, b: &TfIdfVector) -> f64 {
    a.0.iter().zip(&b.0).map(|(x, y)| x * y).sum::<f32>() as f64
}
