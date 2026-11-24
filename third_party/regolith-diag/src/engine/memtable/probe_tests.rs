//! Tests for the range-tombstone lock-free gate and the sequence-only
//! version probe `MemTable::latest_version` shares with `MemTable::get`.

use super::*;
use proptest::prelude::*;

fn memtable() -> MemTable {
    MemTable::new(&MemTableConfig::default()).expect("memtable")
}

fn probe(key: &[u8], snapshot_seq: u64) -> LookupKey {
    LookupKey::from_prefixed(key, snapshot_seq)
}

#[test]
fn covering_seq_is_zero_before_any_range_delete_and_found_after_one() {
    let mt = memtable();
    assert_eq!(mt.covering_range_tombstone_seq(b"c", 10), 0);
    mt.delete_range(b"b", b"d", 5);
    assert_eq!(mt.covering_range_tombstone_seq(b"c", 10), 5);
    assert_eq!(mt.covering_range_tombstone_seq(b"a", 10), 0);
    assert_eq!(
        mt.covering_range_tombstone_seq(b"d", 10),
        0,
        "end is exclusive"
    );
    assert_eq!(
        mt.covering_range_tombstone_seq(b"c", 4),
        0,
        "invisible to a snapshot older than the tombstone"
    );
}

#[test]
fn a_tombstone_free_memtable_answers_from_the_gate_alone() {
    let mt = memtable();
    assert!(!mt.has_range_tombstones());
    assert_eq!(mt.covering_range_tombstone_seq(b"k", u64::MAX), 0);
    assert_eq!(mt.newer_range_tombstone(b"a", b"z", 0, u64::MAX), None);
    mt.delete_range(b"a", b"z", 1);
    assert!(mt.has_range_tombstones());
}

/// The rule a commit relies on: a range delete published before a read
/// began is seen by that read, while the writer keeps appending and no
/// reader ever waits for it. The writer publishes how far it has appended
/// with a release store after each append, the way the commit leader
/// publishes the read horizon after applying; a reader that acquires `p`
/// must find tombstone `p`.
///
/// The writer holds its second append back until every reader has looked the
/// first one up, so every reader runs beside the appends on any number of
/// cores: left to the scheduler, a writer on one core can finish all its
/// appends before a reader thread runs once.
#[test]
fn a_read_sees_every_range_delete_published_before_it_while_appends_continue() {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as StdOrdering};
    // Sized for the interpreter under miri: each lookup walks the tombstones
    // appended so far, so the cost is quadratic in this, and 3000 does not
    // finish there in an hour. The race it checks is the same at 40.
    const APPENDS: u64 = if cfg!(miri) { 40 } else { 3_000 };
    const READERS: usize = 4;

    /// Tells the writer a reader failed, so it stops waiting for that reader
    /// and the failure is reported instead of a wait that never ends.
    struct FailedReader<'a>(&'a AtomicBool);
    impl Drop for FailedReader<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.0.store(true, StdOrdering::Release);
            }
        }
    }

    let mt = memtable();
    let published = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    let looked_up = AtomicUsize::new(0);
    let reader_failed = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..READERS {
            scope.spawn(|| {
                let _failed = FailedReader(&reader_failed);
                let mut lookups = 0u64;
                while !done.load(StdOrdering::Acquire) {
                    let p = published.load(StdOrdering::Acquire);
                    if p == 0 {
                        std::thread::yield_now();
                        continue;
                    }
                    let key = format!("k{p:06}").into_bytes();
                    assert_eq!(
                        mt.covering_range_tombstone_seq(&key, u64::MAX),
                        p,
                        "a range delete published before this read was not seen"
                    );
                    lookups += 1;
                    if lookups == 1 {
                        looked_up.fetch_add(1, StdOrdering::Release);
                    }
                }
                assert!(lookups > 0, "a reader never ran a lookup");
            });
        }
        for seq in 1..=APPENDS {
            let start = format!("k{seq:06}").into_bytes();
            let end = format!("k{seq:06}\x00").into_bytes();
            mt.delete_range(&start, &end, seq);
            published.store(seq, StdOrdering::Release);
            if seq == 1 {
                while looked_up.load(StdOrdering::Acquire) < READERS
                    && !reader_failed.load(StdOrdering::Acquire)
                {
                    std::thread::yield_now();
                }
            }
        }
        done.store(true, StdOrdering::Release);
    });
}

#[test]
fn latest_version_agrees_with_get() {
    let mt = memtable();
    mt.put(b"k", b"v1", 1);
    mt.put(b"k", b"v2", 2);
    mt.delete(b"k", 3);
    mt.merge(b"k", b"op", 4);
    for snapshot_seq in 0..=5u64 {
        let lk = probe(b"k", snapshot_seq);
        assert_eq!(
            mt.latest_version(&lk).map(|(seq, _)| seq),
            mt.get(&lk).map(|(seq, _)| seq)
        );
    }
    assert_eq!(mt.latest_version(&probe(b"k", 0)), None);
    assert_eq!(
        mt.latest_version(&probe(b"k", 2)),
        Some((2, VALUE_TYPE_VALUE))
    );
    assert_eq!(
        mt.latest_version(&probe(b"k", 3)),
        Some((3, VALUE_TYPE_DELETION))
    );
    assert_eq!(
        mt.latest_version(&probe(b"k", 4)),
        Some((4, VALUE_TYPE_MERGE))
    );
}

fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(0u8..4, 0..3)
}

proptest! {
    /// About a third of cases push zero tombstones, exercising the
    /// lock-free gate on an empty set; the rest exercise the locked path.
    #[test]
    fn sequence_probe_and_tombstone_gate_match_a_linear_model(
        entries in proptest::collection::vec((key_strategy(), 1u64..40, 0u8..3), 0..48),
        tombstones in proptest::collection::vec((key_strategy(), key_strategy(), 1u64..40), 0..3),
        probes in proptest::collection::vec((key_strategy(), 0u64..45), 1..16),
    ) {
        let mt = memtable();
        let mut model: Vec<(Vec<u8>, u64, u8)> = Vec::new();
        for (key, seq, kind) in &entries {
            match kind {
                0 => mt.put(key, b"v", *seq),
                1 => mt.delete(key, *seq),
                _ => mt.merge(key, b"op", *seq),
            }
            model.push((key.clone(), *seq, *kind));
        }
        // The skip list keeps duplicate (key, seq) entries; `seek_ge`
        // returns the first in list order and the model takes the max
        // seq, which is the same value, so duplicates need no special
        // casing here.
        let mut pushed_tombstones: Vec<(Vec<u8>, Vec<u8>, u64)> = Vec::new();
        for (start, end, seq) in &tombstones {
            if start < end {
                mt.delete_range(start, end, *seq);
                pushed_tombstones.push((start.clone(), end.clone(), *seq));
            }
        }

        for (key, snapshot_seq) in &probes {
            let expected_latest = model
                .iter()
                .filter(|(k, seq, _)| k == key && *seq <= *snapshot_seq)
                .map(|(_, seq, _)| *seq)
                .max();
            let expected_covering = pushed_tombstones
                .iter()
                .filter(|(start, end, seq)| {
                    start.as_slice() <= key.as_slice()
                        && key.as_slice() < end.as_slice()
                        && *seq <= *snapshot_seq
                })
                .map(|(_, _, seq)| *seq)
                .max()
                .unwrap_or(0);

            let lk = probe(key, *snapshot_seq);
            prop_assert_eq!(mt.latest_version(&lk).map(|(seq, _)| seq), expected_latest);
            prop_assert_eq!(mt.get(&lk).map(|(seq, _)| seq), expected_latest);
            prop_assert_eq!(mt.covering_range_tombstone_seq(key, *snapshot_seq), expected_covering);
        }
    }
}
