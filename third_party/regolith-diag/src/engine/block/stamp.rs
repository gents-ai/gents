//! A block whose entries read at one sequence, whatever their keys store.
//!
//! An ingested table is read at the sequence its manifest record carries
//! (D48, `sstable::global_seq`). Its data blocks are cached as stored, with
//! that sequence attached as a stamp: the `!seq` bytes every entry's key
//! reads with in place of its own. A key is reconstructed from the stored
//! bytes, since a key's trailer can be part of the prefix the next key
//! shares, and only the copy a reader sees or compares carries the stamp.

use std::cmp::Ordering;

use super::super::internal_key::{
    INTERNAL_KEY_SUFFIX_LEN, compare_internal_keys, compare_internal_split,
};

/// The bytes of an internal key's sequence: the 8 before its value type.
fn seq_bytes(key: &mut [u8]) -> &mut [u8] {
    let len = key.len();
    &mut key[len - INTERNAL_KEY_SUFFIX_LEN..len - 1]
}

/// Put `stamp` in place of `key`'s sequence and hand back the bytes it
/// replaced, which put back undo it. `key` is a validated internal key.
pub(super) fn swap(key: &mut [u8], stamp: [u8; 8]) -> [u8; 8] {
    let bytes = seq_bytes(key);
    let mut replaced = [0u8; 8];
    replaced.copy_from_slice(bytes);
    bytes.copy_from_slice(&stamp);
    replaced
}

/// How the stored internal key `key`, read with `stamp` (or as stored when
/// `None`), orders against `target`, without copying it.
#[inline]
pub(super) fn compare(key: &[u8], stamp: Option<[u8; 8]>, target: &[u8]) -> Ordering {
    let Some(stamp) = stamp else {
        return compare_internal_keys(key, target);
    };
    let user_end = key.len() - INTERNAL_KEY_SUFFIX_LEN;
    let mut trailer = [0u8; INTERNAL_KEY_SUFFIX_LEN];
    trailer[..8].copy_from_slice(&stamp);
    trailer[8] = key[key.len() - 1];
    compare_internal_split(target, &key[..user_end], &trailer).reverse()
}

/// `key` as it reads with `stamp`, written over `out`.
pub(super) fn read_into(key: &[u8], stamp: [u8; 8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(key);
    swap(out, stamp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::{
        VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, VALUE_TYPE_VALUE, encode_internal_key,
    };
    use proptest::prelude::*;

    proptest! {
        /// Reading a stored key with a stamp orders it against any target
        /// exactly as the key written at the stamp's sequence would be.
        #[test]
        fn a_stamped_key_orders_as_the_key_written_at_the_stamp(
            user_key in proptest::collection::vec(0u8..4, 0..4),
            stored_seq in any::<u64>(),
            seq in any::<u64>(),
            value_type in prop_oneof![
                Just(VALUE_TYPE_DELETION), Just(VALUE_TYPE_VALUE), Just(VALUE_TYPE_MERGE)
            ],
            target_key in proptest::collection::vec(0u8..4, 0..4),
            target_seq in any::<u64>(),
        ) {
            let stored = encode_internal_key(&user_key, stored_seq, value_type);
            let written = encode_internal_key(&user_key, seq, value_type);
            let target = encode_internal_key(&target_key, target_seq, VALUE_TYPE_DELETION);
            let stamp = (!seq).to_be_bytes();
            prop_assert_eq!(
                compare(&stored, Some(stamp), &target),
                compare_internal_keys(&written, &target)
            );
            let mut read = Vec::new();
            read_into(&stored, stamp, &mut read);
            prop_assert_eq!(&read, &written);
            let mut swapped = stored.clone();
            let replaced = swap(&mut swapped, stamp);
            prop_assert_eq!(&swapped, &written);
            swap(&mut swapped, replaced);
            prop_assert_eq!(swapped, stored);
        }
    }
}
