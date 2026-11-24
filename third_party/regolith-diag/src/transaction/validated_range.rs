//! What a validated scan ([`crate::ScanCheck::Range`]) records: the key range
//! it covered, so the commit can refuse any write that landed in it.
//!
//! One [`RangeRecord`] per cursor, registered with the transaction when the
//! cursor is made and moved forward by the cursor at the end of every page.
//! It covers exactly what the caller consumed: from the scan's start to the
//! last key handed out, or to the end of the range once the scan was
//! exhausted. A cursor that stops early validates only what it read. At
//! commit the records become disjoint [`RangeCheck`]s, overlapping and
//! touching ones coalesced.

use std::sync::Arc;

use kovan::Atom;

use crate::engine::RangeCheck;

/// A key range `[lo, hi)` in prefixed keys, empty when `hi <= lo`.
///
/// One cursor moves a record, and only one of its bounds: a forward cursor
/// the upper one, a reverse cursor the lower one. A commit reads the record,
/// possibly from another thread while the cursor is mid-page. Each bound is
/// swapped in whole, so the commit reads a bound the cursor published, and
/// since the other bound never moves, a pair the record really had. Neither
/// side waits for the other.
pub(super) struct RangeRecord {
    lo: Atom<Vec<u8>>,
    hi: Atom<Vec<u8>>,
}

impl RangeRecord {
    /// A record covering `[lo, hi)`.
    pub(super) fn new(lo: Vec<u8>, hi: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            lo: Atom::new(lo),
            hi: Atom::new(hi),
        })
    }

    /// Move the exclusive upper bound, for a forward cursor.
    pub(super) fn set_hi(&self, hi: &[u8]) {
        self.hi.store(hi.to_vec());
    }

    /// Move the inclusive lower bound, for a reverse cursor.
    pub(super) fn set_lo(&self, lo: &[u8]) {
        self.lo.store(lo.to_vec());
    }

    fn get(&self) -> (Vec<u8>, Vec<u8>) {
        (self.lo.load_clone(), self.hi.load_clone())
    }
}

/// The records as the ranges a commit validates against `observed_seq`:
/// empty ones dropped, the rest sorted and disjoint.
pub(super) fn checks(records: &[Arc<RangeRecord>], observed_seq: u64) -> Vec<RangeCheck> {
    let mut spans: Vec<(Vec<u8>, Vec<u8>)> = records
        .iter()
        .map(|record| record.get())
        .filter(|(lo, hi)| lo < hi)
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<RangeCheck> = Vec::with_capacity(spans.len());
    for (lo, hi) in spans {
        match merged.last_mut() {
            Some(prev) if lo <= prev.hi => prev.hi = prev.hi.clone().max(hi),
            _ => merged.push(RangeCheck {
                lo,
                hi,
                observed_seq,
            }),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(lo: &[u8], hi: &[u8]) -> Arc<RangeRecord> {
        RangeRecord::new(lo.to_vec(), hi.to_vec())
    }

    fn spans(checks: &[RangeCheck]) -> Vec<(&[u8], &[u8])> {
        checks
            .iter()
            .map(|c| (c.lo.as_slice(), c.hi.as_slice()))
            .collect()
    }

    #[test]
    fn overlapping_and_touching_records_coalesce_and_empty_ones_drop() {
        let records = [
            record(b"m", b"p"),
            record(b"b", b"d"),
            record(b"p", b"q"),
            record(b"c", b"e"),
            record(b"x", b"x"),
            record(b"z", b"y"),
        ];
        let got = checks(&records, 7);
        assert_eq!(
            spans(&got),
            [(&b"b"[..], &b"e"[..]), (&b"m"[..], &b"q"[..])]
        );
        assert!(got.iter().all(|check| check.observed_seq == 7));
    }

    #[test]
    fn a_record_moved_by_its_cursor_is_read_as_moved() {
        let forward = record(b"a", b"a");
        assert!(checks(&[Arc::clone(&forward)], 1).is_empty());
        forward.set_hi(b"f\0");
        assert_eq!(spans(&checks(&[forward], 1)), [(&b"a"[..], &b"f\0"[..])]);

        let reverse = record(b"z", b"z");
        reverse.set_lo(b"q");
        assert_eq!(spans(&checks(&[reverse], 1)), [(&b"q"[..], &b"z"[..])]);
    }

    /// A commit reading a record while its cursor moves it, on another
    /// thread: every bound read is one the cursor published, whole, and the
    /// bound a forward cursor never moves is always the one it began with.
    #[test]
    fn a_record_read_while_its_cursor_moves_it_is_never_torn() {
        // The `i`th bound: `i + 1` bytes, each of them `i`.
        let bound = |i: u8| vec![i; usize::from(i) + 1];
        let moves: u8 = if cfg!(miri) { 40 } else { 200 };
        for _ in 0..if cfg!(miri) { 2 } else { 50 } {
            let record = RangeRecord::new(b"lo".to_vec(), bound(0));
            let done = std::sync::atomic::AtomicBool::new(false);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    for i in 1..=moves {
                        record.set_hi(&bound(i));
                    }
                    done.store(true, std::sync::atomic::Ordering::Release);
                });
                let mut newest = 0u8;
                loop {
                    let finished = done.load(std::sync::atomic::Ordering::Acquire);
                    let (lo, hi) = record.get();
                    assert_eq!(lo, b"lo");
                    let i = hi[0];
                    assert_eq!(hi, bound(i), "a bound the cursor never published");
                    assert!(i >= newest, "the bound went back from {newest} to {i}");
                    newest = i;
                    if finished {
                        break;
                    }
                }
                assert_eq!(newest, moves, "the last read sees the last bound");
            });
        }
    }
}
