use proptest::prelude::*;

proptest! {
    #[test]
    fn skeleton_always_true(x in 0u32..100) {
        prop_assert!(x < 100);
    }
}
