//! Distances over signed int8 embeddings stored as byte blobs.

/// Exact integer-domain L2. Both operands use signed two's-complement bytes.
/// A dimension mismatch is invalid; identical vectors have exactly zero distance.
pub fn l2_i8(a: &[u8], b: &[u8]) -> Option<f64> {
    if a.len() != b.len() {
        return None;
    }
    let squared: u64 = a
        .iter()
        .zip(b)
        .map(|(&a, &b)| {
            let difference = i64::from(a as i8) - i64::from(b as i8);
            (difference * difference) as u64
        })
        .sum();
    Some((squared as f64).sqrt())
}
