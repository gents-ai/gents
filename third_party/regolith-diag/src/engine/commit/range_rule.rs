//! The commit check of a validated scan: did any write land inside the range
//! it covered after its snapshot?
//!
//! A put, a delete, a merge operand or a range delete newer than the snapshot
//! inside the range is a change, whether it touched a key the scan yielded or
//! one it walked past. The check asks every source of the view for an entry of
//! the range above the snapshot. It never merges sources or reads a value, but
//! it does read every block of the range, so it costs about one more scan of
//! it. That scan runs on the committing thread ahead of the pipeline, up to
//! a sampled horizon; deciding the slot asks only the memtables for what
//! landed above it, and the tables too only when one took a newer sequence
//! since or the horizon's check was left open (`early.rs`, `txn.rs`).
//
// vertexia: a table keeps no sequence range, so every table that overlaps the
// range is read. Recording each table's newest sequence in the manifest would
// let the walk skip tables written before the snapshot. The walk up to a
// sampled horizon already runs ahead of the pipeline (`early.rs`).

use std::io;

use super::super::internal_key::write_kind;
use super::super::manifest::overlapping;
use super::super::{ReadView, RegolithEngine, with_key_scratch};
use crate::WriteKind;

impl RegolithEngine {
    /// A write above `floor` inside `[lo, hi)` (prefixed keys) as the key it
    /// landed on, its sequence and its kind, or `None` when the range is as
    /// the snapshot left it. Sources are asked newest first, so the answer
    /// is the same on every run.
    pub(super) fn written_in_range(
        &self,
        lo: &[u8],
        hi: &[u8],
        floor: u64,
        view: &ReadView,
    ) -> io::Result<Option<(Vec<u8>, u64, WriteKind)>> {
        self.written_in_range_of(lo, hi, floor, u64::MAX, view, false)
    }

    /// [`Self::written_in_range`] among the writes at or below `ceil` in the
    /// memtables, asking only the memtables when `memtables_only` is set:
    /// exact when no table holds a sequence above `floor` (see `early.rs`).
    /// The tables are asked unbounded: a caller with a `ceil` below
    /// `u64::MAX` asks only when no table holds a sequence above it
    /// (`write_free.rs`).
    pub(super) fn written_in_range_of(
        &self,
        lo: &[u8],
        hi: &[u8],
        floor: u64,
        ceil: u64,
        view: &ReadView,
        memtables_only: bool,
    ) -> io::Result<Option<(Vec<u8>, u64, WriteKind)>> {
        if lo >= hi {
            return Ok(None);
        }
        for mt in std::iter::once(&view.active).chain(view.frozen.iter().rev()) {
            if let Some((key, seq, value_type)) = mt.newer_in_range(lo, hi, floor, ceil) {
                return Ok(Some((key, seq, write_kind(value_type))));
            }
            if let Some((key, seq)) = mt.newer_range_tombstone(lo, hi, floor, ceil) {
                return Ok(Some((key, seq, WriteKind::RangeDelete)));
            }
        }
        if memtables_only {
            return Ok(None);
        }
        let levels = &view.version.levels;
        let l0 = levels[0].iter().rev().filter(|file| {
            file.meta.smallest_key.as_slice() < hi && lo <= file.meta.largest_key.as_slice()
        });
        let deeper = levels
            .iter()
            .skip(1)
            .flat_map(|files| overlapping(files, lo, hi));
        for file in l0.chain(deeper) {
            if let Some(tombstone) = file
                .reader
                .range_tombstones()
                .iter()
                .find(|t| t.seq > floor && t.overlaps(lo, hi))
            {
                let key = tombstone.start.as_slice().max(lo).to_vec();
                return Ok(Some((key, tombstone.seq, WriteKind::RangeDelete)));
            }
            let found = with_key_scratch(|buf| {
                file.reader.newer_in_range(lo, hi, floor, buf, &self.cache)
            })?;
            if let Some((key, seq, value_type)) = found {
                return Ok(Some((key, seq, write_kind(value_type))));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};
    use crate::{Db, Options};

    const KEYS: [&[u8]; 6] = [b"a", b"b", b"c", b"d", b"e", b"f"];
    const WRITABLE: usize = 5;

    /// Keeps the newest operand; the walk under test never reads a value.
    struct Keep;

    impl crate::MergeOperator for Keep {
        fn name(&self) -> &'static str {
            "keep"
        }

        fn full_merge(&self, _: &[u8], _: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
            operands.last().map(|operand| operand.to_vec())
        }
    }

    #[derive(Clone, Debug)]
    enum Op {
        Put(usize),
        Delete(usize),
        Merge(usize),
        DeleteRange(usize, usize),
        Flush,
        Compact,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (0..WRITABLE).prop_map(Op::Put),
            2 => (0..WRITABLE).prop_map(Op::Delete),
            3 => (0..WRITABLE).prop_map(Op::Merge),
            2 => (0..WRITABLE)
                .prop_flat_map(|lo| (Just(lo), lo + 1..KEYS.len()))
                .prop_map(|(lo, hi)| Op::DeleteRange(lo, hi)),
            2 => Just(Op::Flush),
            1 => Just(Op::Compact),
        ]
    }

    impl Op {
        /// Whether this write lands inside the key indexes `[lo, hi)`.
        fn lands_in(&self, lo: usize, hi: usize) -> bool {
            match *self {
                Self::Put(k) | Self::Delete(k) | Self::Merge(k) => (lo..hi).contains(&k),
                Self::DeleteRange(from, to) => from < hi && lo < to,
                Self::Flush | Self::Compact => false,
            }
        }

        fn kind(&self) -> WriteKind {
            match self {
                Self::Put(_) => WriteKind::Put,
                Self::Delete(_) => WriteKind::Delete,
                Self::Merge(_) => WriteKind::Merge,
                Self::DeleteRange(..) => WriteKind::RangeDelete,
                Self::Flush | Self::Compact => unreachable!("not a write"),
            }
        }
    }

    proptest! {
        // Each case opens a database, so far fewer than the default 256.
        #![proptest_config(ProptestConfig::with_cases(96))]

        /// A range holds a newer write, at or below a ceiling at or above every
        /// sequence a table holds, exactly when the write log says one landed
        /// inside it between the floor and the ceiling, and the write named is
        /// one that did.
        #[test]
        fn a_range_reports_a_write_exactly_when_one_landed_in_it(
            ops in proptest::collection::vec(op(), 0..40),
        ) {
            let dir = TempDir::new().unwrap();
            let db = Db::open(
                dir.path(),
                Options::default().merge_operator(Some(Arc::new(Keep))),
            )
            .unwrap();
            let mut log: Vec<(u64, Op)> = Vec::new();
            // A snapshot before the first write and after every write keeps
            // every entry and tombstone alive: no tombstone retires (E27), and
            // every floor below is one a snapshot protects, as a
            // transaction's are.
            let mut snapshots = vec![db.snapshot()];
            for op in &ops {
                match *op {
                    Op::Put(k) => db.put(KEYS[k], b"v").unwrap(),
                    Op::Delete(k) => db.delete(KEYS[k]).unwrap(),
                    Op::Merge(k) => db.merge(KEYS[k], b"op").unwrap(),
                    Op::DeleteRange(lo, hi) => db.delete_range(KEYS[lo], KEYS[hi]).unwrap(),
                    Op::Flush => {
                        db.flush().unwrap();
                        continue;
                    }
                    Op::Compact => {
                        db.compact_range(None, None).wait().unwrap();
                        continue;
                    }
                }
                log.push((db.latest_sequence(), op.clone()));
                snapshots.push(db.snapshot());
            }

            let engine = db.engine();
            let view = engine.view.load();
            let key_of = |i: usize| prefix_key(DEFAULT_CF_ID, KEYS[i]);
            for lo in 0..KEYS.len() {
                for hi in lo..=KEYS.len() {
                    // The end of the key space is "just past the last key".
                    let hi_key = if hi == KEYS.len() { prefix_key(DEFAULT_CF_ID, b"g") } else { key_of(hi) };
                    // No ceiling, then every ceiling the contract allows: at or
                    // above the newest sequence a table holds.
                    let ceilings = std::iter::once(u64::MAX)
                        .chain(view.version.last_seq..=db.latest_sequence() + 1);
                    for ceil in ceilings {
                    for floor in 0..=db.latest_sequence() + 1 {
                        let found = engine
                            .written_in_range_of(&key_of(lo), &hi_key, floor, ceil, &view, false)
                            .unwrap();
                        let landed = |seq: u64, op: &Op| {
                            seq > floor && seq <= ceil && op.lands_in(lo, hi)
                        };
                        let any = lo < hi && log.iter().any(|(seq, op)| landed(*seq, op));
                        prop_assert_eq!(
                            found.is_some(), any, "[{}, {}) in ({}, {}]", lo, hi, floor, ceil
                        );
                        if let Some((key, seq, kind)) = found {
                            let named = log.iter().any(|(s, op)| {
                                landed(*s, op) && *s == seq && op.kind() == kind
                            });
                            prop_assert!(named, "the write named ({:?}, {}) is not in the log", kind, seq);
                            prop_assert!(
                                key >= key_of(lo) && key < hi_key,
                                "the key named lies inside the range"
                            );
                        }
                    }
                    }
                }
            }
        }
    }
}
