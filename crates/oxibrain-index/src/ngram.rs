//! Character n-gram primitives: the single language-independent similarity
//! primitive that serves four uses (ARCHITECTURE.md §7.3):
//!
//! - Fallback vectors (character n-grams replace the English word tokenizer)
//! - Fuzzy name similarity (n-gram Jaccard replaces Jaro-Winkler, §7.7)
//! - Resolution blocking (MinHash/LSH candidate generation, M9 §10.1)
//! - (FTS trigram is a SQLite tokenizer, not computed here)
//!
//! **P11 / §18 rule 6:** this module contains no word list, no stemmer, and no
//! script check. It is the only crate permitted to contain such a thing, and it
//! does not need one — character n-grams are script-neutral by construction.

use std::collections::BTreeMap;

/// Boundary sentinel used to pad shingles so that word/surface boundaries are
/// preserved. `\u{0}` is chosen because normalized entity surfaces never contain
/// it, and shingles are only ever hashed or compared — never stored as visible
/// text.
const BOUNDARY: char = '\u{0}';

/// Character n-gram shingles over a normalized string.
///
/// Language-independent by construction (P11): no word boundaries, no stemming.
/// Pads with `n-1` boundary sentinels on each side so short strings still
/// produce shingles and boundary position information is retained.
///
/// For `n = 0` or an empty string, returns an empty set.
pub fn shingles(s: &str, n: usize) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;
    if s.is_empty() || n == 0 {
        return BTreeSet::new();
    }
    let pad = n.saturating_sub(1);
    let chars: Vec<char> = std::iter::repeat(BOUNDARY)
        .take(pad)
        .chain(s.chars())
        .chain(std::iter::repeat(BOUNDARY).take(pad))
        .collect();
    if chars.len() < n {
        // Only possible if s was empty, already handled — but guard anyway.
        return BTreeSet::new();
    }
    (0..=chars.len() - n)
        .map(|i| chars[i..i + n].iter().collect())
        .collect()
}

/// Jaccard similarity over shingle sets. Order-insensitive, prefix-neutral (§7.7).
///
/// Returns `|A ∩ B| / |A ∪ B|`. For two empty sets, returns `0.0`.
pub fn jaccard(
    a: &std::collections::BTreeSet<String>,
    b: &std::collections::BTreeSet<String>,
) -> f64 {
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Shannon entropy over the character distribution of the shingle set (§10.1).
///
/// Boundary sentinels are excluded so the entropy reflects the actual content
/// diversity. Low entropy (e.g. a string of repeated characters in any script)
/// signals that the shingle set is unreliable for fuzzy matching. This is the
/// gate that prevents false-positive merges on short or repetitive surfaces.
pub fn shingle_entropy(sh: &std::collections::BTreeSet<String>) -> f64 {
    let mut freq: BTreeMap<char, u64> = BTreeMap::new();
    let mut total: u64 = 0;
    for shingle in sh {
        for c in shingle.chars().filter(|&c| c != BOUNDARY) {
            *freq.entry(c).or_default() += 1;
            total += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    freq.values()
        .map(|&count| {
            let p = count as f64 / total as f64;
            -p * p.log2()
        })
        .sum()
}

/// Deterministic MinHash signature. Seeded, fixed permutation count.
///
/// Each permutation hashes every shingle with a distinct seed and keeps the
/// minimum. The resulting signature estimates Jaccard similarity in sublinear
/// time for candidate generation (M9 §10.1). For an empty shingle set, every
/// slot is `u64::MAX`.
pub fn minhash(sh: &std::collections::BTreeSet<String>, perms: usize) -> Vec<u64> {
    if sh.is_empty() {
        return vec![u64::MAX; perms];
    }
    (0..perms)
        .map(|i| {
            // Golden-ratio seed mixing — distinct permutation per slot.
            let seed = (i as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(0x517C_C1B7_2722_0A95);
            sh.iter()
                .map(|s| seeded_fnv1a(s, seed))
                .min()
                .unwrap_or(u64::MAX)
        })
        .collect()
}

/// LSH bands over a MinHash signature, for candidate generation (M9 §10.1).
///
/// Splits the signature into `⌊len / band_size⌋` bands and hashes each band to
/// a `u64`. Two signatures that agree on at least one band are candidates. The
/// band size controls the precision/recall trade-off.
pub fn lsh_bands(sig: &[u64], band_size: usize) -> Vec<u64> {
    if sig.is_empty() || band_size == 0 {
        return Vec::new();
    }
    sig.chunks(band_size)
        .map(|band| {
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            for &v in band {
                h ^= v;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        })
        .collect()
}

// ─── Internal helpers ─────────────────────────────────────────────────────

/// FNV-1a with a seed offset. Deterministic across platforms.
fn seeded_fnv1a(s: &str, seed: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    // ─── Basic correctness ────────────────────────────────────────────────

    #[test]
    fn shingles_basic_latin() {
        let sh = shingles("hello", 3);
        // Padded: \0\0hello\0\0 → windows of 3
        // \0\0h \0he hel ell llo lo\0 o\0\0
        assert!(sh.contains("\u{0}\u{0}h"));
        assert!(sh.contains("hel"));
        assert!(sh.contains("ell"));
        assert!(sh.contains("llo"));
        assert!(sh.contains("o\u{0}\u{0}"));
        assert_eq!(sh.len(), 7);
    }

    #[test]
    fn shingles_short_string() {
        // A 1-char string with n=3 still produces shingles.
        let sh = shingles("a", 3);
        assert!(!sh.is_empty());
        assert!(sh.contains("\u{0}a\u{0}"));
    }

    #[test]
    fn shingles_empty() {
        assert!(shingles("", 3).is_empty());
        assert!(shingles("abc", 0).is_empty());
    }

    #[test]
    fn shingles_n1() {
        let sh = shingles("abc", 1);
        assert_eq!(sh.len(), 3); // {a, b, c}
        assert!(sh.contains("a") && sh.contains("b") && sh.contains("c"));
    }

    #[test]
    fn shingles_cjk() {
        // CJK characters are individual chars in Rust — n-grams work natively.
        let sh = shingles("张伟", 2);
        // Padded: \0张伟\0 → windows of 2: \0张 张伟 伟\0
        assert_eq!(sh.len(), 3);
        assert!(sh.contains("张伟"));
    }

    #[test]
    fn jaccard_identity() {
        let sh = shingles("hello", 3);
        assert_eq!(jaccard(&sh, &sh), 1.0);
    }

    #[test]
    fn jaccard_empty_set() {
        let sh = shingles("hello", 3);
        let empty = BTreeSet::new();
        assert_eq!(jaccard(&sh, &empty), 0.0);
        assert_eq!(jaccard(&empty, &sh), 0.0);
    }

    #[test]
    fn jaccard_both_empty() {
        let empty = BTreeSet::new();
        assert_eq!(jaccard(&empty, &empty), 0.0);
    }

    #[test]
    fn jaccard_symmetric() {
        let a = shingles("hello", 3);
        let b = shingles("world", 3);
        assert!((jaccard(&a, &b) - jaccard(&b, &a)).abs() < 1e-12);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a = shingles("abcde", 3);
        let b = shingles("acbde", 3); // swap positions 1,2
        // 3 common shingles out of 11 total → 3/11 ≈ 0.2727
        let j = jaccard(&a, &b);
        assert!(j > 0.0 && j < 1.0);
        // Verify the exact value for this deterministic case.
        assert!((j - 3.0 / 11.0).abs() < 1e-10, "got {j}");
    }

    // ─── Script invariance (P11) ──────────────────────────────────────────

    #[test]
    fn script_invariance_single_swap() {
        // P11: a single adjacent-character swap must affect Jaccard similarly
        // regardless of writing system. This is the test that encodes language
        // independence for this module.
        //
        // With 5 distinct characters and n=3, a single swap at position 1 changes
        // exactly 4 of 7 shingles in every script, giving Jaccard = 3/11.
        let n = 3;
        let cases: &[(&str, &str)] = &[
            ("abcde", "acbde"),           // Latin
            ("가나다라마", "가다나라마"), // Hangul
            ("金木水火土", "金水木火土"), // Han
            ("ضصثقف", "ضثصقف"),           // Arabic
        ];

        let jaccards: Vec<f64> = cases
            .iter()
            .map(|(a, b)| jaccard(&shingles(a, n), &shingles(b, n)))
            .collect();

        let max = jaccards.iter().cloned().fold(0.0f64, f64::max);
        let min = jaccards.iter().cloned().fold(1.0f64, f64::min);
        let spread = max - min;

        assert!(
            spread < 1e-10,
            "script invariance violated: spread={spread}, values={jaccards:?}"
        );
    }

    // ─── Entropy ──────────────────────────────────────────────────────────

    #[test]
    fn entropy_low_for_repeated_chars() {
        let sh = shingles("aaaa", 3);
        let e = shingle_entropy(&sh);
        // Only one distinct character (excluding boundary) → entropy 0.
        assert!(e < 0.01, "expected ~0, got {e}");
    }

    #[test]
    fn entropy_higher_for_diverse_chars() {
        let low = shingle_entropy(&shingles("aaaa", 3));
        let high = shingle_entropy(&shingles("abcd", 3));
        assert!(high > low, "diverse should have higher entropy");
    }

    #[test]
    fn entropy_zero_for_empty() {
        assert_eq!(shingle_entropy(&BTreeSet::new()), 0.0);
    }

    // ─── MinHash ──────────────────────────────────────────────────────────

    #[test]
    fn minhash_deterministic() {
        let sh = shingles("hello world", 3);
        let sig1 = minhash(&sh, 32);
        let sig2 = minhash(&sh, 32);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn minhash_empty() {
        let sig = minhash(&BTreeSet::new(), 16);
        assert_eq!(sig.len(), 16);
        assert!(sig.iter().all(|&v| v == u64::MAX));
    }

    #[test]
    fn minhash_similar_sets_similar_signatures() {
        // Two sets with high Jaccard should have many matching MinHash slots.
        let a = shingles("abcdefgh", 3);
        let b = shingles("abcdefgh", 3);
        let sig_a = minhash(&a, 128);
        let sig_b = minhash(&b, 128);
        let matches = sig_a.iter().zip(&sig_b).filter(|(x, y)| x == y).count();
        assert_eq!(matches, 128); // identical sets → all match

        // A different string should have fewer matches.
        let c = shingles("xyz12345", 3);
        let sig_c = minhash(&c, 128);
        let matches_c = sig_a.iter().zip(&sig_c).filter(|(x, y)| x == y).count();
        assert!(
            matches_c < 128,
            "different sets should not match everywhere"
        );
    }

    // ─── LSH bands ────────────────────────────────────────────────────────

    #[test]
    fn lsh_bands_count() {
        let sig = minhash(&shingles("hello", 3), 20);
        let bands = lsh_bands(&sig, 5);
        assert_eq!(bands.len(), 4); // 20 / 5 = 4 bands
    }

    #[test]
    fn lsh_bands_empty() {
        assert!(lsh_bands(&[], 4).is_empty());
        assert!(lsh_bands(&[1, 2, 3], 0).is_empty());
    }

    #[test]
    fn lsh_bands_remainder() {
        // 22 elements, band_size 5 → 4 full bands + 1 partial (2 elements).
        let sig: Vec<u64> = (0..22).collect();
        let bands = lsh_bands(&sig, 5);
        assert_eq!(bands.len(), 5);
    }

    // ─── Property tests (proptest) ────────────────────────────────────────

    proptest! {
        #[test]
        fn prop_jaccard_identity(s in "[a-z]{3,20}") {
            let sh = shingles(&s, 3);
            prop_assert_eq!(jaccard(&sh, &sh), 1.0);
        }

        #[test]
        fn prop_jaccard_empty(s in "[a-z]{3,20}") {
            let sh = shingles(&s, 3);
            let empty = BTreeSet::new();
            prop_assert_eq!(jaccard(&sh, &empty), 0.0);
        }

        #[test]
        fn prop_jaccard_symmetry(a in "[a-z]{3,20}", b in "[a-z]{3,20}") {
            let sa = shingles(&a, 3);
            let sb = shingles(&b, 3);
            prop_assert!((jaccard(&sa, &sb) - jaccard(&sb, &sa)).abs() < 1e-12);
        }

        #[test]
        fn prop_jaccard_range(a in "[a-z]{3,20}", b in "[a-z]{3,20}") {
            let sa = shingles(&a, 3);
            let sb = shingles(&b, 3);
            let j = jaccard(&sa, &sb);
            prop_assert!((0.0..=1.0).contains(&j));
        }

        #[test]
        fn prop_shingles_nonempty(s in ".{1,30}") {
            let sh = shingles(&s, 3);
            prop_assert!(!sh.is_empty(), "shingles must not be empty for non-empty input");
        }

        #[test]
        fn prop_minhash_deterministic(s in "[a-z]{3,30}") {
            let sh = shingles(&s, 3);
            let sig1 = minhash(&sh, 16);
            let sig2 = minhash(&sh, 16);
            prop_assert_eq!(sig1, sig2);
        }

        #[test]
        fn prop_minhash_length(sh in "[a-z]{3,30}", perms in 1usize..=64) {
            let sh = shingles(&sh, 3);
            let sig = minhash(&sh, perms);
            prop_assert_eq!(sig.len(), perms);
        }
    }
}
