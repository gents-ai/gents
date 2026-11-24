//! An ingested table, read at the one sequence its manifest record carries.
//!
//! An ingest installs an external table as it is, copied or linked, and
//! records in the manifest the sequence it drew for it (D48). The file's own
//! entries keep whatever sequence they were written with (`SstFileWriter`
//! writes 0); every read sees the recorded one instead. Three seams carry
//! that, so no read path has to know:
//!
//! - a data block is stamped with the recorded sequence when it is decoded,
//!   before the block cache holds it, and every walk, iterator, validation
//!   and compaction input over a stamped block reads each key with the stamp
//!   in place of the sequence it stores (`block::stamp`), with no copy of
//!   the block;
//! - an index seek, whose keys are the file's own last keys, is steered by
//!   the target's user key and by whether the recorded sequence is visible to
//!   it, never by the stored sequences;
//! - the range tombstones, held in memory, are rewritten when the reader is
//!   bound to the sequence.
//!
//! Reading every entry at one sequence needs the file to hold at most one
//! entry per user key, which the ingest checks: two entries of one key at
//! one sequence have no order.
//! A compaction reads the file through these seams and writes what it reads,
//! so its output carries the sequence in its entries and no record of its own.

use super::super::internal_key::{INTERNAL_KEY_SUFFIX_LEN, decode_internal_key};
use super::super::range_tombstone::RangeTombstone;

/// The trailer that sorts before every entry of a user key.
const BEFORE_KEY: [u8; INTERNAL_KEY_SUFFIX_LEN] = [0; INTERNAL_KEY_SUFFIX_LEN];

/// The trailer that sorts after every entry of a user key.
const AFTER_KEY: [u8; INTERNAL_KEY_SUFFIX_LEN] = [0xFF; INTERNAL_KEY_SUFFIX_LEN];

/// Where an index seek for `target` lands in a table read at `seq`, as a
/// user key and the trailer to seek it with. `None` when `target` is too
/// short to be an internal key, which only the unit tests' raw blocks are.
///
/// Every entry of the target's user key reads as `(user_key, seq)`. With
/// `seq` at or below the target's sequence the seek goes to the key's first
/// entry, which is then at or after the target (at equal sequences the value
/// type decides, and the block walk that follows compares each entry in
/// full). With `seq` above it, every entry of the key reads as newer than the
/// target, so the seek goes past them all. The stored sequences never enter
/// into it, which is what keeps a file written at sequences above `seq` from
/// steering the seek into the wrong block.
pub(super) fn steer(target: &[u8], seq: u64) -> Option<(&[u8], [u8; INTERNAL_KEY_SUFFIX_LEN])> {
    if target.len() < INTERNAL_KEY_SUFFIX_LEN {
        return None;
    }
    let (user_key, target_seq, _) = decode_internal_key(target);
    Some((
        user_key,
        if seq <= target_seq {
            BEFORE_KEY
        } else {
            AFTER_KEY
        },
    ))
}

/// `tombstones` with every sequence replaced by `seq`.
pub(super) fn tombstones_at(tombstones: &[RangeTombstone], seq: u64) -> Vec<RangeTombstone> {
    tombstones
        .iter()
        .map(|rt| RangeTombstone::new(rt.start.clone(), rt.end.clone(), seq))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::block::{Block, BlockBuilder, RESTART_INTERVAL};
    use crate::engine::internal_key::{
        VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, VALUE_TYPE_VALUE, compare_internal_keys,
        compare_internal_split, encode_internal_key,
    };
    use proptest::prelude::*;
    use std::ops::ControlFlow;

    fn block_of(entries: &[(Vec<u8>, Vec<u8>)]) -> Block {
        let mut builder = BlockBuilder::new(RESTART_INTERVAL);
        for (key, value) in entries {
            builder.add(key, value);
        }
        Block::decode_data_block(builder.finish()).unwrap()
    }

    /// Every entry `scan_from(target)` hands out, key and value.
    fn scan(block: &Block, target: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut key_buf = Vec::new();
        let mut out = Vec::new();
        let ended: Option<()> = block.scan_from(target, &mut key_buf, |key, off, len| {
            out.push((key.to_vec(), block.entry_bytes(off, len).unwrap().to_vec()));
            ControlFlow::Continue(())
        });
        assert!(ended.is_none());
        out
    }

    #[test]
    fn a_stamped_block_scans_like_the_block_written_at_the_sequence() {
        // `ab` is a prefix of `ab\xff...`, so the second key shares bytes of
        // the first one's trailer: a stamp left in place would corrupt it.
        let mut stored = vec![
            (
                encode_internal_key(b"ab", 0, VALUE_TYPE_VALUE),
                b"1".to_vec(),
            ),
            (
                encode_internal_key(b"ab\xff\xff", 0, VALUE_TYPE_DELETION),
                Vec::new(),
            ),
            (
                encode_internal_key(b"b", 900, VALUE_TYPE_MERGE),
                b"3".to_vec(),
            ),
        ];
        stored.extend((0..40u32).map(|i| {
            let key = format!("k{i:03}").into_bytes();
            (encode_internal_key(&key, 5, VALUE_TYPE_VALUE), key)
        }));
        let written: Vec<_> = stored
            .iter()
            .map(|(key, value)| {
                let (user_key, _, value_type) = decode_internal_key(key);
                (encode_internal_key(user_key, 42, value_type), value.clone())
            })
            .collect();
        let stamped = block_of(&stored).stamped(42);
        let reference = block_of(&written);
        for (key, _) in &written {
            let user_key = decode_internal_key(key).0;
            for snapshot in [0, 41, 42, 43, u64::MAX] {
                let target = encode_internal_key(user_key, snapshot, VALUE_TYPE_DELETION);
                assert_eq!(scan(&stamped, &target), scan(&reference, &target));
            }
        }
        for (key, _) in &stored {
            let mut out = Vec::new();
            let (user_key, _, value_type) = decode_internal_key(key);
            let read = encode_internal_key(user_key, 42, value_type);
            assert_eq!(stamped.read_key(key, &mut out), read.as_slice());
            assert_eq!(stamped.owned_key(key), read);
            assert_eq!(block_of(&stored).read_key(key, &mut out), key.as_slice());
        }
    }

    #[test]
    fn tombstones_take_the_sequence() {
        let rewritten = tombstones_at(&[RangeTombstone::new(b"a".to_vec(), b"c".to_vec(), 3)], 9);
        assert_eq!(rewritten.len(), 1);
        assert_eq!(
            (
                rewritten[0].start.as_slice(),
                rewritten[0].end.as_slice(),
                rewritten[0].seq
            ),
            (&b"a"[..], &b"c"[..], 9)
        );
    }

    proptest! {
        /// The steered seek and a seek on the file as if it were written at
        /// `seq` agree on every stored key: an index key, which is a stored
        /// key, sorts below the steered target exactly when the same key
        /// written at `seq` sorts below the real target, whatever sequence it
        /// was stored at. That is the whole contract an index seek needs.
        #[test]
        fn a_steered_seek_orders_every_stored_key_as_if_written_at_the_sequence(
            stored_key in proptest::collection::vec(0u8..4, 0..4),
            stored_seq in any::<u64>(),
            target_key in proptest::collection::vec(0u8..4, 0..4),
            target_seq in any::<u64>(),
            seq in 1u64..u64::MAX,
            value_type in prop_oneof![
                Just(VALUE_TYPE_DELETION), Just(VALUE_TYPE_VALUE), Just(VALUE_TYPE_MERGE)
            ],
        ) {
            prop_assume!(seq != target_seq);
            let stored = encode_internal_key(&stored_key, stored_seq, value_type);
            let read_as = encode_internal_key(&stored_key, seq, value_type);
            let target = encode_internal_key(&target_key, target_seq, VALUE_TYPE_DELETION);
            let (user_key, trailer) = steer(&target, seq).unwrap();
            prop_assert_eq!(
                compare_internal_split(&stored, user_key, &trailer).is_lt(),
                compare_internal_keys(&read_as, &target).is_lt()
            );
        }
    }

    #[test]
    fn at_an_equal_sequence_the_seek_starts_at_the_key() {
        let target = encode_internal_key(b"k", 5, VALUE_TYPE_VALUE);
        let (user_key, trailer) = steer(&target, 5).unwrap();
        for stored_seq in [0, 5, u64::MAX] {
            let stored = encode_internal_key(b"k", stored_seq, VALUE_TYPE_DELETION);
            assert!(
                !compare_internal_split(&stored, user_key, &trailer).is_lt(),
                "every entry of the key sorts at or after the steered target"
            );
        }
        assert!(steer(b"short", 5).is_none());
    }
}
