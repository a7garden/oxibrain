//! Resolution blocking (M9 §10.1): MinHash/LSH candidate generation + entropy
//! gate. Pure — no I/O, no rusqlite.
//!
//! Resolving a mention against a large entity set must not scan every candidate.
//! MinHash signatures over character n-gram shingles, bucketed by LSH bands,
//! give a sublinear candidate set: two similar surfaces collide on at least one
//! band with high probability (controlled by the band count). The entropy gate
//! rejects low-entropy shingle sets (short or repetitive surfaces) where fuzzy
//! matching is unreliable and a false-positive merge is most likely.

use crate::ngram;
use std::collections::{BTreeSet, HashMap};

/// Blocking parameters. `band_size` controls the precision/recall trade-off:
/// smaller bands → more recall (more false candidates), larger bands → higher
/// precision (fewer false candidates but more false negatives).
#[derive(Debug, Clone, Copy)]
pub struct BlockingConfig {
    /// MinHash permutation count (signature width).
    pub permutations: usize,
    /// LSH band size (signature is split into `⌊perms / band_size⌋` bands).
    pub band_size: usize,
    /// Minimum shingle entropy for a surface to participate (§10.1).
    pub min_entropy: f64,
}

impl Default for BlockingConfig {
    fn default() -> Self {
        Self {
            permutations: 128,
            // 128/4 = 32 bands. Band size 4 gives ~0.87 recall at Jaccard 0.5 —
            // high recall for name blocking, which must not miss the true match.
            band_size: 4,
            min_entropy: 0.5,
        }
    }
}

/// A sublinear candidate index over shingle sets, keyed by LSH band hashes.
///
/// Positions (`usize`) are indices into the slice passed to [`LshIndex::build`],
/// so callers map them back to their own keys without any allocation here.
pub struct LshIndex {
    bands: HashMap<u64, Vec<usize>>,
    perms: usize,
    band_size: usize,
}

impl LshIndex {
    /// Build the index from per-entity shingle sets. Low-entropy sets are
    /// skipped — they are never returned as candidates and never matched
    /// fuzzily.
    pub fn build(shingle_sets: &[BTreeSet<String>], config: &BlockingConfig) -> Self {
        let mut bands: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, sh) in shingle_sets.iter().enumerate() {
            if !entropy_gate(sh, config.min_entropy) {
                continue;
            }
            let sig = ngram::minhash(sh, config.permutations);
            for band in ngram::lsh_bands(&sig, config.band_size) {
                bands.entry(band).or_default().push(i);
            }
        }
        Self {
            bands,
            perms: config.permutations,
            band_size: config.band_size,
        }
    }

    /// Candidate positions sharing at least one LSH band with the query.
    /// Sublinear in the number of indexed sets: only the query's bands are
    /// probed, never the whole index. Empty for low-entropy queries.
    pub fn candidates(&self, query: &BTreeSet<String>, min_entropy: f64) -> Vec<usize> {
        if !entropy_gate(query, min_entropy) {
            return Vec::new();
        }
        let sig = ngram::minhash(query, self.perms);
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for band in ngram::lsh_bands(&sig, self.band_size) {
            if let Some(idxs) = self.bands.get(&band) {
                seen.extend(idxs.iter().copied());
            }
        }
        seen.into_iter().collect()
    }

    /// Incrementally insert a single position into the index. O(1) per call:
    /// computes the MinHash signature for `shingles` and adds band entries
    /// pointing at `position`. Low-entropy sets are skipped, consistent with
    /// [`build`](Self::build). Used by the resolution cache to avoid a full
    /// O(N) rebuild when a new entity key is added.
    pub fn insert(&mut self, shingles: &BTreeSet<String>, position: usize) {
        let config = BlockingConfig::default();
        if !entropy_gate(shingles, config.min_entropy) {
            return;
        }
        let sig = ngram::minhash(shingles, self.perms);
        for band in ngram::lsh_bands(&sig, self.band_size) {
            self.bands.entry(band).or_default().push(position);
        }
    }
}

/// Entropy gate (§10.1): low-entropy shingle sets are unreliable for fuzzy
/// matching (repetitive or extremely short surfaces).
pub fn entropy_gate(shingles: &BTreeSet<String>, min_entropy: f64) -> bool {
    ngram::shingle_entropy(shingles) >= min_entropy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shingles(s: &str) -> BTreeSet<String> {
        ngram::shingles(s, 3)
    }

    #[test]
    fn similar_surfaces_collide_on_a_band() {
        let config = BlockingConfig::default();
        let surfaces = [
            "alice smith",
            "bob jones",
            "carol baker",
            "dave miller",
            "eve davis",
            "frank wilson",
            "grace lee",
            "alicia smith", // near-duplicate of "alice smith"
        ];
        let sets: Vec<BTreeSet<String>> = surfaces.iter().map(|s| shingles(s)).collect();
        let index = LshIndex::build(&sets, &config);

        // Query with a slightly misspelled "alice smith" surface.
        let candidates = index.candidates(&shingles("alise smith"), config.min_entropy);
        // "alice smith" (0) and "alicia smith" (7) should be among candidates.
        assert!(
            candidates.contains(&0),
            "alice smith should be a candidate: {candidates:?}"
        );
        assert!(
            candidates.contains(&7),
            "alicia smith should be a candidate: {candidates:?}"
        );
        // The candidate set must be smaller than the full index (sublinear).
        assert!(candidates.len() < sets.len());
    }

    #[test]
    fn entropy_gate_rejects_repetitive_surfaces() {
        let config = BlockingConfig::default();
        assert!(!entropy_gate(&shingles("aaaaa"), config.min_entropy));
        assert!(!entropy_gate(&shingles("ㅋㅋㅋㅋㅋ"), config.min_entropy));
        assert!(entropy_gate(&shingles("alice smith"), config.min_entropy));
    }

    #[test]
    fn empty_query_yields_no_candidates() {
        let config = BlockingConfig::default();
        let sets = vec![shingles("alice smith"), shingles("bob jones")];
        let index = LshIndex::build(&sets, &config);
        assert!(
            index
                .candidates(&BTreeSet::new(), config.min_entropy)
                .is_empty()
        );
    }
}
