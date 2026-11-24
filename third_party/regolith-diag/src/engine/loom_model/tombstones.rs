//! Models of a memtable's range-tombstone log (T1 to T4 in
//! `engine::memtable::tombstones`), run against the real log through the
//! real `MemTable`: under `--cfg loom` its atomics and its slot cells are
//! loom's, so a slot read that is not ordered after the slot's write fails
//! the model as a race, and a length published before the slot is written
//! is exactly such a read.
//!
//! The log has no index in a loom build (T4): every read is the tail scan
//! over the published prefix, which is the protocol T2 is about.

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::{explore, explore_bounded, memtable};
use crate::engine::range_tombstone::RangeTombstone;

/// A reader racing two appends sees a prefix of them: whenever it sees the
/// second tombstone it sees the first, and every tombstone it lists is
/// whole (a torn one would be a loom race on its slot).
pub fn a_reader_sees_a_prefix_of_whole_appends() {
    explore(
        "a_reader_sees_a_prefix_of_whole_appends",
        16,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let writer = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.delete_range(b"a", b"c", 1);
                    mt.delete_range(b"m", b"p", 2);
                })
            };
            let reader = {
                let mt = Arc::clone(&mt);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let second = mt.covering_range_tombstone_seq(b"n", u64::MAX);
                    let first = mt.covering_range_tombstone_seq(b"b", u64::MAX);
                    if second == 2 {
                        assert_eq!(first, 1, "a reader saw the second append without the first");
                        witness.record();
                    }
                    let listed = mt.clone_range_tombstones();
                    let seqs: Vec<u64> = listed.iter().map(|t| t.seq).collect();
                    assert!(
                        seqs.is_empty() || seqs == [1] || seqs == [1, 2],
                        "a reader listed {seqs:?}, which is not a prefix of the appends"
                    );
                })
            };
            writer.join().expect("writer");
            reader.join().expect("reader");
        },
    );
}

/// The rule a commit relies on: a range delete applied before the read
/// horizon that includes it is published is seen by every read that
/// acquires that horizon. The horizon here is the release store a commit
/// leader makes after applying, and the acquire load a reader samples.
pub fn a_published_range_delete_is_seen_by_every_later_read() {
    explore(
        "a_published_range_delete_is_seen_by_every_later_read",
        4,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let horizon = Arc::new(AtomicU64::new(0));
            let leader = {
                let (mt, horizon) = (Arc::clone(&mt), Arc::clone(&horizon));
                loom::thread::spawn(move || {
                    mt.delete_range(b"a", b"z", 1);
                    horizon.store(1, Ordering::Release);
                })
            };
            let reader = {
                let (mt, horizon) = (Arc::clone(&mt), Arc::clone(&horizon));
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let snapshot = horizon.load(Ordering::Acquire);
                    let covering = mt.covering_range_tombstone_seq(b"k", snapshot);
                    if snapshot == 1 {
                        assert_eq!(covering, 1, "a published range delete was not seen");
                        witness.record();
                    } else {
                        assert_eq!(covering, 0, "a read saw a delete above its snapshot");
                    }
                })
            };
            leader.join().expect("leader");
            reader.join().expect("reader");
        },
    );
}

/// The defect T2 rules out, in the log's own shape: one slot and a length,
/// where the append publishes the length before it writes the slot.
struct EarlyLength {
    slot: UnsafeCell<u64>,
    len: AtomicUsize,
}

/// Calibration for [`a_reader_sees_a_prefix_of_whole_appends`]: a reader
/// that trusts a length published before its tombstone was written reads
/// the slot while the writer is still writing it, which loom reports as a
/// race. If this passed, the models above would say nothing about why the
/// log writes the slot first.
pub fn a_length_published_before_its_tombstone_is_caught() {
    explore(
        "a_length_published_before_its_tombstone_is_caught",
        2,
        1,
        |witness| {
            let log = Arc::new(EarlyLength {
                slot: UnsafeCell::new(0),
                len: AtomicUsize::new(0),
            });
            let writer = {
                let log = Arc::clone(&log);
                loom::thread::spawn(move || {
                    log.len.store(1, Ordering::Release);
                    log.slot.with_mut(|slot| {
                        // SAFETY: the defect under test; loom tracks the
                        // cell and fails the model on the unordered access.
                        #[allow(unsafe_code)]
                        unsafe {
                            *slot = 7
                        };
                    });
                })
            };
            let reader = {
                let log = Arc::clone(&log);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    if log.len.load(Ordering::Acquire) == 1 {
                        witness.record();
                        // SAFETY: as above.
                        #[allow(unsafe_code)]
                        let seen = log.slot.with(|slot| unsafe { *slot });
                        assert_eq!(seen, 7, "a reader saw an append before its contents");
                    }
                })
            };
            writer.join().expect("writer");
            reader.join().expect("reader");
        },
    );
}

/// T1: two threads append at once, as two helpers of the commit pipeline
/// applying two slots do. Each append is handed a position of its own, so
/// both tombstones are listed, whole.
///
/// Bound: 3 preemptions.
pub fn two_appends_at_once_are_both_listed() {
    explore_bounded(
        "two_appends_at_once_are_both_listed",
        Some(3),
        8,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let appenders: Vec<_> = [(&b"a"[..], &b"c"[..], 1u64), (b"m", b"p", 2)]
                .into_iter()
                .map(|(start, end, seq)| {
                    let mt = Arc::clone(&mt);
                    loom::thread::spawn(move || mt.delete_range(start, end, seq))
                })
                .collect();
            for appender in appenders {
                appender.join().expect("appender");
            }
            witness.record();
            let seqs: Vec<u64> = mt.clone_range_tombstones().iter().map(|t| t.seq).collect();
            assert_eq!(seqs.len(), 2, "an append was lost: {seqs:?}");
            assert_eq!(mt.covering_range_tombstone_seq(b"b", u64::MAX), 1);
            assert_eq!(mt.covering_range_tombstone_seq(b"n", u64::MAX), 2);
        },
    );
}

/// The same tombstone appended by two threads at once, as two helpers
/// applying one slot do, is listed once and deletes what it deleted.
///
/// Bound: 3 preemptions.
pub fn one_tombstone_appended_twice_is_listed_once() {
    explore_bounded(
        "one_tombstone_appended_twice_is_listed_once",
        Some(3),
        8,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let appenders: Vec<_> = (0..2)
                .map(|_| {
                    let mt = Arc::clone(&mt);
                    loom::thread::spawn(move || mt.delete_range(b"a", b"c", 1))
                })
                .collect();
            for appender in appenders {
                appender.join().expect("appender");
            }
            witness.record();
            assert_eq!(
                mt.clone_range_tombstones().len(),
                1,
                "one tombstone was listed twice"
            );
            assert_eq!(mt.covering_range_tombstone_seq(b"b", u64::MAX), 1);
        },
    );
}

/// T1 for a batch: two tombstones appended as one batch
/// (`MemTable::delete_ranges`, the way recovery brings them) beside a
/// single append on another thread. Each of the three is handed a position
/// of its own, so all are listed, whole, and each deletes what it deletes.
///
/// Bound: 3 preemptions.
pub fn a_batch_beside_an_append_loses_neither() {
    explore_bounded(
        "a_batch_beside_an_append_loses_neither",
        Some(3),
        8,
        1,
        |witness| {
            let mt = Arc::new(memtable());
            let batch = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || {
                    mt.delete_ranges(vec![
                        RangeTombstone::new(b"e".to_vec(), b"g".to_vec(), 1),
                        RangeTombstone::new(b"a".to_vec(), b"c".to_vec(), 1),
                    ]);
                })
            };
            let single = {
                let mt = Arc::clone(&mt);
                loom::thread::spawn(move || mt.delete_range(b"m", b"p", 2))
            };
            batch.join().expect("batch");
            single.join().expect("single");
            witness.record();
            let listed = mt.clone_range_tombstones();
            assert_eq!(listed.len(), 3, "an append was lost: {listed:?}");
            assert_eq!(mt.covering_range_tombstone_seq(b"b", u64::MAX), 1);
            assert_eq!(mt.covering_range_tombstone_seq(b"f", u64::MAX), 1);
            assert_eq!(mt.covering_range_tombstone_seq(b"n", u64::MAX), 2);
            assert_eq!(mt.covering_range_tombstone_seq(b"d", u64::MAX), 0);
        },
    );
}

/// Calibration for [`two_appends_at_once_are_both_listed`]: a position
/// taken by a load and a store. Both appends read the same length and are
/// handed the same position.
pub fn a_position_taken_by_a_load_and_a_store_is_handed_out_twice() {
    explore_bounded(
        "a_position_taken_by_a_load_and_a_store_is_handed_out_twice",
        Some(3),
        2,
        1,
        |witness| {
            let claimed = Arc::new(AtomicUsize::new(0));
            let appenders: Vec<_> = (0..2)
                .map(|_| {
                    let claimed = Arc::clone(&claimed);
                    loom::thread::spawn(move || {
                        let position = claimed.load(Ordering::Acquire);
                        claimed.store(position + 1, Ordering::Release);
                        position
                    })
                })
                .collect();
            let positions: Vec<usize> = appenders
                .into_iter()
                .map(|appender| appender.join().expect("appender"))
                .collect();
            witness.record();
            assert_ne!(
                positions[0], positions[1],
                "two appends were handed one position"
            );
        },
    );
}
