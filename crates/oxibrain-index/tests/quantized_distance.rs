use oxibrain_index::quantized_distance::l2_i8;
use proptest::prelude::*;

#[test]
fn sign_bits_and_fractional_embedding_components_are_not_discarded() {
    let a = oxibrain_index::quantize_i8_unit(&[0.02, -0.03, 0.04, -0.01]);
    let b = oxibrain_index::quantize_i8_unit(&[-0.02, 0.03, -0.04, 0.01]);
    assert_eq!(l2_i8(&a, &a), Some(0.0));
    assert!(l2_i8(&a, &b).unwrap() > 10.0);
    assert_eq!(l2_i8(&[255], &[0]), Some(1.0));
    assert_eq!(l2_i8(&[128], &[127]), Some(255.0));
    assert_eq!(l2_i8(&[], &[0]), None);
}
proptest! {
    #[test]
    fn signed_reference_symmetry_and_identity(pairs in prop::collection::vec((any::<i8>(),any::<i8>()),0..2048)) {
        let a:Vec<_> = pairs.iter().map(|(a,_)| *a as u8).collect();
        let b:Vec<_> = pairs.iter().map(|(_,b)| *b as u8).collect();
        let reference = pairs.iter().map(|(a,b)| (f64::from(*a)-f64::from(*b)).powi(2)).sum::<f64>().sqrt();
        prop_assert_eq!(l2_i8(&a,&b),Some(reference));
        prop_assert_eq!(l2_i8(&a,&b),l2_i8(&b,&a));
        prop_assert_eq!(l2_i8(&a,&a),Some(0.0));
    }
}
