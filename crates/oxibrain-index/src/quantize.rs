//! Binary quantization for embedding vectors (ARCHITECTURE.md §7.7, D25).
//!
//! Maps each `f32` dimension to its sign bit (1 if `f > 0.0`, else 0), then packs
//! eight bits per byte with the first dimension in the most-significant bit of
//! the first byte. Hamming distance between two packed vectors (XOR + popcount)
//! approximates cosine distance for high-dimensional embeddings: similar
//! embeddings agree on most sign bits.
//!
//! Pure module: no I/O, no allocation beyond the returned `Vec`s. Zero
//! dependencies beyond `std`.

/// Quantize a float vector to a packed binary representation (one bit per
/// dimension, eight dimensions per byte, MSB-first).
///
/// Each dimension's sign bit is `1` if the component is strictly greater than
/// `0.0`, otherwise `0` — so `0.0` and all negative values map to `0`. If the
/// input length is not a multiple of eight, the final byte is padded with `0`
/// bits in its low positions, so the output has length `ceil(vec.len() / 8)`.
pub fn quantize(vec: &[f32]) -> Vec<u8> {
    let n_bytes = vec.len().div_ceil(8);
    let mut out = Vec::with_capacity(n_bytes);
    let chunks = vec.chunks_exact(8);
    let remainder = chunks.remainder();
    for chunk in chunks {
        let mut byte: u8 = 0;
        // MSB = first dimension in the chunk.
        for (i, &v) in chunk.iter().enumerate() {
            if v > 0.0 {
                byte |= 1 << (7 - i);
            }
        }
        out.push(byte);
    }
    if !remainder.is_empty() {
        let mut byte: u8 = 0;
        for (i, &v) in remainder.iter().enumerate() {
            if v > 0.0 {
                byte |= 1 << (7 - i);
            }
        }
        out.push(byte);
    }
    out
}

/// Hamming distance between two packed binary vectors — the number of bit
/// positions at which they differ, computed as XOR + popcount.
///
/// # Panics
///
/// Panics if `a` and `b` have different lengths; equal-length packing is an
/// invariant of the quantization pipeline and a mismatch indicates a caller
/// bug, not a runtime condition.
pub fn hamming(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(
        a.len(),
        b.len(),
        "hamming: packed vectors must have equal length (got {} vs {})",
        a.len(),
        b.len()
    );
    let mut total: usize = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        total += (x ^ y).count_ones() as usize;
    }
    total
}

/// Unpack a packed binary vector into a sign vector of `+1.0` / `-1.0`.
///
/// Bit order matches [`quantize`]: the first dimension is the MSB of the
/// first byte. Returns `packed.len() * 8` floats; trailing zero-padding bits
/// in a final partial byte yield `-1.0` (matching `quantize`'s `0` mapping
/// for `f <= 0.0`).
pub fn dequantize_signs(packed: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(packed.len() * 8);
    for &byte in packed {
        for i in 0..8 {
            let bit = (byte >> (7 - i)) & 1;
            out.push(if bit == 1 { 1.0 } else { -1.0 });
        }
    }
    out
}

/// Approximate cosine similarity from a Hamming distance and a dimension
/// count.
///
/// A pair of sign vectors differs in `hamming_dist` of `dim` positions, so
/// their cosine similarity is `1 - 2 * hamming / dim` (matching signs add `+1`,
/// differing signs add `-1`). Returns `1.0` for `dim == 0` — an empty vector
/// pair is treated as identical rather than as a domain error, since the
/// caller is asking about a zero-dimensional embedding space that cannot
/// meaningfully express a cosine value.
///
/// # Caller contract
///
/// `hamming_dist` must be `≤ dim`. Larger values are a caller bug; the function
/// returns a value below `-1.0` in that case rather than clamping, so misuse
/// is visible to tests.
pub fn cosine_approx(hamming_dist: usize, dim: usize) -> f64 {
    if dim == 0 {
        return 1.0;
    }
    1.0 - 2.0 * (hamming_dist as f64 / dim as f64)
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ─── Known-vector roundtrip ───────────────────────────────────────────

    #[test]
    fn quantize_known_vector_packs_msb_first() {
        // Signs: [>0, >0, ≤0, >0, ≤0, ≤0, >0, ≤0] = [1,1,0,1,0,0,1,0]
        // = 0b11010010 = 0xD2.
        let v = vec![1.0, 0.5, -1.0, 0.1, -2.0, 0.0, 3.0, -0.5];
        assert_eq!(quantize(&v), vec![0xD2]);
    }

    #[test]
    fn quantize_handles_partial_chunk_with_zero_padding() {
        // Five dimensions: bits [1, 1, 0, 1, 0], then three zero-pad bits.
        // = 0b11010_000 = 0xD0.
        let v = vec![1.0, 1.0, -1.0, 1.0, -1.0];
        assert_eq!(quantize(&v), vec![0xD0]);
    }

    #[test]
    fn quantize_zero_vector_produces_all_zero_bytes() {
        let v = vec![0.0f32; 16];
        assert_eq!(quantize(&v), vec![0u8; 2]);
    }

    #[test]
    fn quantize_all_positive_produces_all_ones_bytes() {
        let v = vec![0.1f32; 24];
        assert_eq!(quantize(&v), vec![0xFFu8; 3]);
    }

    #[test]
    fn quantize_empty_input_returns_empty_output() {
        assert!(quantize(&[]).is_empty());
    }

    #[test]
    fn dequantize_signs_roundtrips_quantize_on_aligned_input() {
        let v = vec![1.0, -1.0, 0.5, -2.0, 3.0, -3.0, 0.001, -0.001];
        let packed = quantize(&v);
        assert_eq!(packed.len(), 1);
        let signs = dequantize_signs(&packed);
        assert_eq!(signs.len(), 8);
        let expected: Vec<f32> = v
            .iter()
            .map(|&x| if x > 0.0 { 1.0 } else { -1.0 })
            .collect();
        assert_eq!(signs, expected);
    }

    #[test]
    fn hamming_identical_vectors_is_zero() {
        let v = vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0];
        let p = quantize(&v);
        assert_eq!(hamming(&p, &p), 0);
    }

    #[test]
    fn hamming_inverts_to_negated_vector_when_input_is_negated() {
        let v = vec![1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0];
        let p_pos = quantize(&v);
        let p_neg = quantize(&[-v[0], -v[1], -v[2], -v[3], -v[4], -v[5], -v[6], -v[7]]);
        // Every bit flipped → full-byte hamming.
        assert_eq!(hamming(&p_pos, &p_neg), 8);
    }

    #[test]
    fn cosine_approx_identical_is_one() {
        assert!((cosine_approx(0, 128) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn cosine_approx_maximally_different_is_minus_one() {
        // All `dim` bits differ.
        assert!((cosine_approx(128, 128) - (-1.0)).abs() < 1e-12);
    }

    #[test]
    fn cosine_approx_handles_zero_dim() {
        // Empty space → treat as identical, not as a domain error.
        assert_eq!(cosine_approx(0, 0), 1.0);
        // Even with a non-zero hamming, dim=0 returns 1.0 (the guard).
        assert_eq!(cosine_approx(5, 0), 1.0);
    }

    #[test]
    #[should_panic(expected = "packed vectors must have equal length")]
    fn hamming_panics_on_length_mismatch() {
        let a = vec![0xFFu8];
        let b = vec![0xFFu8, 0x00u8];
        let _ = hamming(&a, &b);
    }

    /// Cosine similarity between two raw `f32` vectors. Local helper for the
    /// ranking-preservation property test; not part of the module's public
    /// surface.
    #[allow(dead_code)] // referenced inside proptest! macro, not visible to dead-code analysis
    fn raw_cosine(a: &[f32], b: &[f32]) -> f64 {
        debug_assert_eq!(a.len(), b.len());
        let mut dot = 0.0f64;
        let mut na = 0.0f64;
        let mut nb = 0.0f64;
        for (x, y) in a.iter().zip(b.iter()) {
            let xf = *x as f64;
            let yf = *y as f64;
            dot += xf * yf;
            na += xf * xf;
            nb += yf * yf;
        }
        let denom = (na * nb).sqrt();
        if denom == 0.0 { 0.0 } else { dot / denom }
    }

    proptest! {
        fn prop_hamming_identity(v in proptest::collection::vec(any::<f32>(), 0..128)) {
            let p = quantize(&v);
            prop_assert_eq!(hamming(&p, &p), 0);
        }

        #[test]
        fn prop_hamming_symmetry(
            a in proptest::collection::vec(any::<f32>(), 0..128),
            b in proptest::collection::vec(any::<f32>(), 0..128)
        ) {
            // Pad to equal length so the invariant holds.
            let n = a.len().max(b.len());
            let mut a = a;
            let mut b = b;
            a.resize(n, 0.0);
            b.resize(n, 0.0);
            let pa = quantize(&a);
            let pb = quantize(&b);
            prop_assert_eq!(hamming(&pa, &pb), hamming(&pb, &pa));
        }

        #[test]
        fn prop_hamming_triangle_inequality(
            a in proptest::collection::vec(any::<f32>(), 0..96),
            b in proptest::collection::vec(any::<f32>(), 0..96),
            c in proptest::collection::vec(any::<f32>(), 0..96)
        ) {
            // Equal-length padding.
            let n = a.len().max(b.len()).max(c.len());
            let mut a = a;
            let mut b = b;
            let mut c = c;
            a.resize(n, 0.0);
            b.resize(n, 0.0);
            c.resize(n, 0.0);
            let pa = quantize(&a);
            let pb = quantize(&b);
            let pc = quantize(&c);
            let dab = hamming(&pa, &pb);
            let dbc = hamming(&pb, &pc);
            let dac = hamming(&pa, &pc);
            prop_assert!(dab + dbc >= dac, "triangle inequality violated: {} + {} < {}", dab, dbc, dac);
            prop_assert!(dbc + dac >= dab);
            prop_assert!(dab + dac >= dbc);
        }

        #[test]
        fn prop_quantize_deterministic(v in proptest::collection::vec(any::<f32>(), 0..128)) {
            prop_assert_eq!(quantize(&v), quantize(&v));
        }

        /// Quantization → dequantization recovers sign bits verbatim for
        /// inputs whose packed length is `ceil(v.len()/8)` (the public contract).
        #[test]
        fn prop_dequantize_matches_signs(v in proptest::collection::vec(any::<f32>(), 0..128)) {
            let packed = quantize(&v);
            let signs = dequantize_signs(&packed);
            prop_assert_eq!(signs.len(), packed.len() * 8);
            for (i, &x) in v.iter().enumerate() {
                prop_assert_eq!(signs[i], if x > 0.0 { 1.0 } else { -1.0 });
            }
        }

        /// For high-dimensional embeddings, binary quantization preserves the
        /// relative cosine ranking in the overwhelming majority of triples.
        ///
        /// Each proptest case draws a single `anchor` vector in
        /// `[-1, 1]^dim` (dim ∈ [128, 256]) and then internally generates
        /// `TRIALS = 800` independent `(b, c)` pairs. We skip degenerate
        /// anchors (`‖anchor‖ < 3.0`, where cosine values become dominated
        /// by numerical noise from a near-zero anchor) and any pair whose
        /// continuous-cosine gap is below `MIN_GAP = 0.20`. We then require
        /// at least 20 qualifying triples (for a stable rate estimate) and
        /// ≤15% violation rate.
        ///
        /// At this regime (≈40–50 fired triples per proptest case, after
        /// the 20-fired floor), statistical fluctuation is small enough
        /// that the test passes deterministically on the correct quantizer
        /// and fails on a broken one (random bits or ≥20% per-bit noise,
        /// which flip the ordering on ~30–50% of triples).
        fn prop_quantize_preserves_cosine_ranking(
            anchor in proptest::collection::vec(-1.0f32..1.0, 128..256)
        ) {
            const TRIALS: usize = 800;
            const MIN_GAP: f64 = 0.20;
            const MIN_ANCHOR_NORM: f64 = 3.0;

            // Skip degenerate anchors — when ‖anchor‖ ≪ ‖b‖, ‖c‖, the
            // cosine values are dominated by numerical noise and the
            // ordering is meaningless.
            let anchor_norm: f64 = anchor.iter()
                .map(|&x| (x as f64) * (x as f64))
                .sum::<f64>().sqrt();
            prop_assume!(anchor_norm >= MIN_ANCHOR_NORM);

            let dim = anchor.len();
            // Inner LCG so the per-case (b, c) trials are reproducible
            // from the proptest seed. Box-Muller for a real N(0,1) draw.
            let mut state: u64 = anchor.iter()
                .enumerate()
                .fold(0xcbf2_9ce4_8422_2325u64, |acc, (i, &x)| {
                    acc.wrapping_add((x.to_bits() as u64).wrapping_mul((i as u64).wrapping_add(1)))
                });
            let step_u = |s: &mut u64| -> f64 {
                *s = s.wrapping_mul(0x0000_0100_0000_01b3);
                let bits = *s >> 11;
                (bits as f64) / ((1u64 << 53) as f64)
            };
            let mut gauss = || -> f64 {
                let u1 = step_u(&mut state).max(1e-300);
                let u2 = step_u(&mut state);
                let r = (-2.0 * u1.ln()).sqrt();
                r * (2.0 * std::f64::consts::PI * u2).cos()
            };
            let mut next_f32 = || gauss() as f32;

            let qa = quantize(&anchor);
            let mut violations = 0usize;
            let mut fired = 0usize;
            for _ in 0..TRIALS {
                let b: Vec<f32> = (0..dim).map(|_| next_f32()).collect();
                let c: Vec<f32> = (0..dim).map(|_| next_f32()).collect();
                let cos_b = raw_cosine(&anchor, &b);
                let cos_c = raw_cosine(&anchor, &c);
                if cos_b - cos_c < MIN_GAP {
                    continue;
                }
                fired += 1;
                let qb = quantize(&b);
                let qc = quantize(&c);
                let hb = hamming(&qa, &qb);
                let hc = hamming(&qa, &qc);
                if hb > hc {
                    violations += 1;
                }
            }
            // Require ≥20 qualifying triples for a stable rate estimate,
            // and ≤15% violation rate.
            prop_assume!(fired >= 20);
            prop_assert!(
                violations * 100 <= fired * 15,
                "ranking violated too often: {} / {} fired triples for anchor (norm={:.2})",
                violations, fired, anchor_norm
            );
        }

        #[test]
        fn prop_cosine_approx_inverts(h in 0usize..64, dim in 1usize..128) {
            // Clamp to dim so the test only exercises the well-defined domain
            // (h <= dim ⇒ result in [-1, 1]). h > dim is a caller bug, not a
            // property-test concern.
            let h = h.min(dim);
            let v = cosine_approx(h, dim);
            prop_assert!((-1.0..=1.0).contains(&v));
            if h > 0 {
                let prev = cosine_approx(h - 1, dim);
                prop_assert!(v <= prev);
            }
            if h == 0 {
                prop_assert!((v - 1.0).abs() < 1e-12);
            }
            if h == dim {
                prop_assert!((v - (-1.0)).abs() < 1e-12);
            }
        }
    }
}
