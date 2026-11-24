//! Every call the engine makes of the caller's [`MergeOperator`].
//!
//! The engine keys an entry by the caller's key behind a column-family
//! prefix. The operator is the caller's code, and a database with several
//! value shapes dispatches on the key inside it, so each call here hands the
//! operator the key as the caller wrote it: the key [`crate::KeyClassifier`],
//! [`crate::Conflict::key`] and [`crate::LogLayout`] see. A read, a scan, a
//! transaction's read of its own merges, a commit's checks and compaction
//! all reach the operator through this module and nowhere else, which is
//! what keeps the key the same on every path.

use std::io;

use crate::column_family::callers_key;
use crate::options::MergeOperator;

/// [`MergeOperator::full_merge`] for the entry the engine keys `prefixed`.
pub(crate) fn full_merge(
    op: &dyn MergeOperator,
    prefixed: &[u8],
    base: Option<&[u8]>,
    operands: &[&[u8]],
) -> Option<Vec<u8>> {
    op.full_merge(callers_key(prefixed), base, operands)
}

/// [`MergeOperator::partial_merge`] for the entry the engine keys `prefixed`.
pub(crate) fn partial_merge(
    op: &dyn MergeOperator,
    prefixed: &[u8],
    left: &[u8],
    right: &[u8],
) -> Option<Vec<u8>> {
    op.partial_merge(callers_key(prefixed), left, right)
}

/// [`MergeOperator::touches`] for the entry the engine keys `prefixed`.
pub(crate) fn touches(
    op: &dyn MergeOperator,
    prefixed: &[u8],
    operand: &[u8],
    parts: &[u32],
) -> bool {
    op.touches(callers_key(prefixed), operand, parts)
}

/// Fold `operands`, oldest first, onto `base` with `op`, for a read. A point
/// read, a scan and a transaction's read of its own merges all go through
/// here, so all fail the same way when the operator declines.
pub(crate) fn apply_merge(
    op: &dyn MergeOperator,
    prefixed: &[u8],
    base: Option<&[u8]>,
    operands: &[&[u8]],
) -> io::Result<Vec<u8>> {
    full_merge(op, prefixed, base, operands).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("merge operator {} failed for key", op.name()),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::column_family::prefix_key;

    /// Records the key of every call, and declines a merge.
    #[derive(Default)]
    struct Keys(Mutex<Vec<Vec<u8>>>);

    impl Keys {
        fn saw(&self, key: &[u8]) {
            self.0.lock().unwrap().push(key.to_vec());
        }
    }

    impl MergeOperator for Keys {
        fn name(&self) -> &'static str {
            "keys"
        }

        fn full_merge(&self, key: &[u8], _: Option<&[u8]>, _: &[&[u8]]) -> Option<Vec<u8>> {
            self.saw(key);
            None
        }

        fn partial_merge(&self, key: &[u8], _: &[u8], _: &[u8]) -> Option<Vec<u8>> {
            self.saw(key);
            None
        }

        fn touches(&self, key: &[u8], _: &[u8], _: &[u32]) -> bool {
            self.saw(key);
            true
        }
    }

    #[test]
    fn every_call_hands_over_the_key_without_the_family_prefix() {
        let op = Keys::default();
        for family in [1, 2, u32::MAX - 1] {
            let prefixed = prefix_key(family, b"doc/7");
            assert_eq!(full_merge(&op, &prefixed, None, &[b"x"]), None);
            assert_eq!(partial_merge(&op, &prefixed, b"x", b"y"), None);
            assert!(touches(&op, &prefixed, b"x", &[0]));
            assert!(apply_merge(&op, &prefixed, None, &[b"x"]).is_err());
        }
        // An empty caller's key is still a key.
        assert_eq!(full_merge(&op, &prefix_key(1, b""), None, &[b"x"]), None);
        let mut expected = vec![b"doc/7".to_vec(); 12];
        expected.push(Vec::new());
        assert_eq!(*op.0.lock().unwrap(), expected);
    }

    /// The failure a read reports names the operator and no key.
    #[test]
    fn a_declined_merge_is_reported_without_the_key() {
        let op = Keys::default();
        let err = apply_merge(&op, &prefix_key(1, b"SECRET"), None, &[b"x"]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "merge operator keys failed for key");
    }
}
