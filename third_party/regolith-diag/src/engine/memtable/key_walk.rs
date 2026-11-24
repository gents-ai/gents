//! The walk over one key's entries in a memtable, shared by the point probes,
//! the merge-chain read and the commit check's terminator walk. It reads
//! nodes in place; `sstable::key_walk` is its twin for one table.

use std::ops::ControlFlow;

use super::super::merge_chain::VersionSink;
use super::{DbSlice, LookupKey, MemTable, NodeRef, VALUE_TYPE_MERGE, decode_internal_key};

/// Visits one entry of a key in a memtable: its node, sequence and value type.
pub(crate) trait VisitNode<'mem, R>:
    FnMut(NodeRef<'mem>, u64, u8) -> ControlFlow<R>
{
}

impl<'mem, R, F> VisitNode<'mem, R> for F where F: FnMut(NodeRef<'mem>, u64, u8) -> ControlFlow<R> {}
use super::super::source_walk::{Above, Skip};

impl MemTable {
    /// Visits `lk`'s entries newest first among those at or below the lookup's
    /// snapshot, until `visit` breaks. `None` when the key's entries end first.
    /// Nodes are read in place.
    #[inline]
    pub(super) fn scan_key<'mem, R>(
        &'mem self,
        lk: &LookupKey,
        mut visit: impl VisitNode<'mem, R>,
    ) -> Option<R> {
        // A key's versions sort newest first and the lookup key carries the
        // lowest value type, so the seek lands on the newest version at or
        // below the snapshot and every one after it is older still.
        let mut node = self.list.seek_ge(lk.internal());
        while let Some(current) = node {
            let (user_key, seq, value_type) = decode_internal_key(current.key());
            if user_key != lk.prefixed_user_key() {
                return None;
            }
            if let ControlFlow::Break(result) = visit(current, seq, value_type) {
                return Some(result);
            }
            node = current.next();
        }
        None
    }

    /// Walk every visible entry for `key` at `snapshot_seq` in
    /// newest-seq-first order, appending `(seq, value_type, bytes)`
    /// tuples onto `out` and stopping at (and including) the first
    /// terminator (`VALUE_TYPE_VALUE` or `VALUE_TYPE_DELETION`).
    /// Returns `true` when a terminator was reached - callers walking
    /// multiple sources use this to decide whether to continue the
    /// walk into the next source.
    ///
    /// Used by the merge-operator read path to collect a chain of
    /// merge operands layered on top of the underlying base value.
    pub(crate) fn collect_merge_chain<S>(&self, lk: &LookupKey, out: &mut S) -> bool
    where
        S: VersionSink,
    {
        self.scan_key(lk, |node, seq, value_type| {
            out.push_version((seq, value_type, self.value_slice(&node)));
            if value_type != VALUE_TYPE_MERGE {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_some()
    }

    /// Skips `lk`'s merge operands above `floor`, newest first among the
    /// entries at or below the lookup's snapshot, and reports where the skip
    /// ended: whether it passed any operand, and the sequence of the entry it
    /// stopped at, a value or deletion above `floor` or any entry at or below
    /// it. The stop is `None` when every such entry here is an operand above
    /// `floor`, or there is none.
    ///
    /// Reads sequences and value types in place: no value is sliced and no
    /// arena `Arc` is cloned.
    pub(crate) fn skip_merges_above(&self, lk: &LookupKey, floor: u64) -> Skip {
        let mut passed = false;
        let stop = self.scan_key(lk, |_, seq, value_type| {
            if seq <= floor || value_type != VALUE_TYPE_MERGE {
                ControlFlow::Break((seq, value_type))
            } else {
                passed = true;
                ControlFlow::Continue(())
            }
        });
        Skip {
            passed,
            stop: stop.map(|(seq, _)| seq),
            stop_type: stop.map_or(0, |(_, value_type)| value_type),
        }
    }
}

impl MemTable {
    /// The first entry, in key order, whose user key lies in `[lo, hi)` and
    /// whose sequence is above `floor` and at or below `ceil`: its user key,
    /// sequence and value type. Reads in place, and walks every entry of the
    /// range, old versions included, since a skip list has no order by
    /// sequence. `u64::MAX` as `ceil` bounds nothing.
    pub(crate) fn newer_in_range(
        &self,
        lo: &[u8],
        hi: &[u8],
        floor: u64,
        ceil: u64,
    ) -> Option<(Vec<u8>, u64, u8)> {
        let start = LookupKey::from_prefixed(lo, u64::MAX);
        let mut node = self.list.seek_ge(start.internal());
        while let Some(current) = node {
            let (user_key, seq, value_type) = decode_internal_key(current.key());
            if user_key >= hi {
                return None;
            }
            if seq > floor && seq <= ceil {
                return Some((user_key.to_vec(), seq, value_type));
            }
            node = current.next();
        }
        None
    }

    /// Whether this memtable holds an entry whose user key lies in
    /// `[first, last]`, or a range tombstone reaching into it, at any
    /// sequence. One seek; the tombstones are read only when there are any.
    pub(crate) fn holds_key_in(&self, first: &[u8], last: &[u8]) -> bool {
        let start = LookupKey::from_prefixed(first, u64::MAX);
        if self
            .list
            .seek_ge(start.internal())
            .is_some_and(|node| decode_internal_key(node.key()).0 <= last)
        {
            return true;
        }
        self.has_range_tombstones()
            && self
                .range_tombstones
                .any(|rt| rt.start.as_slice() <= last && first < rt.end.as_slice())
    }

    /// Visits `lk`'s entries newest first among those at or below the lookup's
    /// snapshot and above `floor`, until `visit` breaks. `visit` sees each
    /// entry's sequence, value type and value, the value as a view of the
    /// arena that keeps it alive.
    pub(crate) fn visit_above<R>(
        &self,
        lk: &LookupKey,
        floor: u64,
        mut visit: impl FnMut(u64, u8, DbSlice) -> ControlFlow<R>,
    ) -> Above<R> {
        match self.scan_key(lk, |node, seq, value_type| {
            if seq <= floor {
                return ControlFlow::Break(None);
            }
            visit(seq, value_type, self.value_slice(&node)).map_break(Some)
        }) {
            None => Above::Exhausted,
            Some(None) => Above::Floor,
            Some(Some(result)) => Above::Broke(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_VALUE};
    use crate::engine::memtable::MemTableConfig;
    use proptest::prelude::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Put,
        Delete,
        Merge,
    }

    /// An empty memtable on the default configuration.
    fn memtable() -> MemTable {
        MemTable::new(&MemTableConfig::default()).expect("memtable")
    }

    /// Record one write of `kind` to `key` at `seq`.
    fn write(mt: &MemTable, key: &[u8], seq: u64, kind: Kind) {
        match kind {
            Kind::Put => mt.put(key, b"v", seq),
            Kind::Delete => mt.delete(key, seq),
            Kind::Merge => mt.merge(key, b"op", seq),
        }
    }

    /// A lookup of `key` at `snapshot_seq`.
    fn probe(key: &[u8], snapshot_seq: u64) -> LookupKey {
        LookupKey::from_prefixed(key, snapshot_seq)
    }

    impl Kind {
        /// The value type an entry of this kind carries in its internal key.
        fn value_type(self) -> u8 {
            match self {
                Self::Put => VALUE_TYPE_VALUE,
                Self::Delete => VALUE_TYPE_DELETION,
                Self::Merge => VALUE_TYPE_MERGE,
            }
        }
    }

    /// `skip_merges_above` for `key` read at `snapshot_seq`, as reported.
    fn skip_typed(mt: &MemTable, key: &[u8], snapshot_seq: u64, floor: u64) -> Skip {
        mt.skip_merges_above(&probe(key, snapshot_seq), floor)
    }

    /// [`skip_typed`] without the stop's value type, which the model test and
    /// `the_stop_names_the_value_type_of_the_entry_it_ended_at` check apart, so
    /// the cases below stay about where the skip ended.
    fn skip(mt: &MemTable, key: &[u8], snapshot_seq: u64, floor: u64) -> Skip {
        Skip {
            stop_type: 0,
            ..skip_typed(mt, key, snapshot_seq, floor)
        }
    }

    /// A skip that passed an operand or not, and stopped at `stop`.
    fn ended(passed: bool, stop: Option<u64>) -> Skip {
        Skip {
            passed,
            stop,
            stop_type: 0,
        }
    }

    #[test]
    fn the_stop_names_the_value_type_of_the_entry_it_ended_at() {
        let mt = memtable();
        write(&mt, b"k", 1, Kind::Put);
        write(&mt, b"k", 2, Kind::Merge);
        write(&mt, b"k", 3, Kind::Delete);
        write(&mt, b"k", 4, Kind::Merge);
        write(&mt, b"j", 5, Kind::Put);
        let stopped = skip_typed(&mt, b"k", u64::MAX, 0);
        assert_eq!(
            (stopped.passed, stopped.stop, stopped.stop_type),
            (true, Some(3), VALUE_TYPE_DELETION)
        );
        let stopped = skip_typed(&mt, b"j", u64::MAX, 0);
        assert_eq!(
            (stopped.stop, stopped.stop_type),
            (Some(5), VALUE_TYPE_VALUE)
        );
        // At the floor the skip stops on whatever entry is there, an operand
        // included.
        let stopped = skip_typed(&mt, b"k", u64::MAX, 4);
        assert_eq!(
            (stopped.stop, stopped.stop_type),
            (Some(4), VALUE_TYPE_MERGE)
        );
    }

    #[test]
    fn scan_key_visits_newest_first_and_stops_at_the_first_break() {
        let mt = memtable();
        write(&mt, b"j", 1, Kind::Put);
        write(&mt, b"k", 2, Kind::Put);
        write(&mt, b"k", 3, Kind::Merge);
        write(&mt, b"k", 4, Kind::Delete);
        write(&mt, b"l", 5, Kind::Put);

        let mut seen = Vec::new();
        let ended = mt.scan_key(&probe(b"k", u64::MAX), |_, seq, value_type| {
            seen.push((seq, value_type));
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(ended, None, "the key's entries end first");
        assert_eq!(
            seen,
            [
                (4, VALUE_TYPE_DELETION),
                (3, VALUE_TYPE_MERGE),
                (2, VALUE_TYPE_VALUE)
            ]
        );

        let mut seen = Vec::new();
        let stopped = mt.scan_key(&probe(b"k", 3), |_, seq, _| {
            seen.push(seq);
            if seq == 3 {
                ControlFlow::Break("stopped")
            } else {
                ControlFlow::Continue(())
            }
        });
        assert_eq!(stopped, Some("stopped"));
        assert_eq!(seen, [3], "entries above the snapshot are not visited");
    }

    #[test]
    fn collect_merge_chain_walks_until_terminator() {
        let mt = memtable();
        mt.put(b"k", b"base", 1);
        mt.merge(b"k", b"a", 2);
        mt.merge(b"k", b"b", 3);

        let mut chain = Vec::new();
        let reached_term = mt.collect_merge_chain(&probe(b"k", 3), &mut chain);
        assert!(reached_term);
        // Newest seq first: b, a, base (terminator).
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].0, 3);
        assert_eq!(chain[2].0, 1);
        assert_eq!(chain[2].1, VALUE_TYPE_VALUE);
    }

    #[test]
    fn collect_merge_chain_stops_at_tombstone() {
        let mt = memtable();
        mt.delete(b"k", 1);
        mt.merge(b"k", b"a", 2);

        let mut chain = Vec::new();
        let reached_term = mt.collect_merge_chain(&probe(b"k", 2), &mut chain);
        assert!(reached_term);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1].1, VALUE_TYPE_DELETION);
    }

    #[test]
    fn collect_merge_chain_returns_false_when_only_merges_visible() {
        let mt = memtable();
        mt.merge(b"k", b"a", 1);
        mt.merge(b"k", b"b", 2);
        let mut chain = Vec::new();
        let terminated = mt.collect_merge_chain(&probe(b"k", 2), &mut chain);
        assert!(!terminated, "pure-merge chain must return false");
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn collect_merge_chain_skips_entries_above_the_snapshot() {
        let mt = memtable();
        mt.put(b"k", b"base", 1);
        mt.merge(b"k", b"a", 2);
        mt.merge(b"k", b"future", 9);
        let mut chain = Vec::new();
        assert!(mt.collect_merge_chain(&probe(b"k", 2), &mut chain));
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].0, 2);
    }

    #[test]
    fn the_skip_runs_down_to_the_base_when_the_floor_is_below_it() {
        let mt = memtable();
        write(&mt, b"k", 1, Kind::Put);
        write(&mt, b"k", 2, Kind::Merge);
        write(&mt, b"k", 3, Kind::Merge);
        assert_eq!(skip(&mt, b"k", u64::MAX, 0), ended(true, Some(1)));
    }

    #[test]
    fn the_skip_stops_at_the_first_entry_at_or_below_the_floor() {
        let mt = memtable();
        write(&mt, b"k", 1, Kind::Put);
        for seq in 2..=10 {
            write(&mt, b"k", seq, Kind::Merge);
        }
        assert_eq!(skip(&mt, b"k", u64::MAX, 5), ended(true, Some(5)));
        assert_eq!(skip(&mt, b"k", u64::MAX, 10), ended(false, Some(10)));
        assert_eq!(skip(&mt, b"k", u64::MAX, 40), ended(false, Some(10)));
    }

    #[test]
    fn a_deletion_above_the_floor_ends_the_skip() {
        let mt = memtable();
        write(&mt, b"k", 1, Kind::Put);
        write(&mt, b"k", 2, Kind::Merge);
        write(&mt, b"k", 3, Kind::Delete);
        write(&mt, b"k", 4, Kind::Merge);
        assert_eq!(skip(&mt, b"k", u64::MAX, 1), ended(true, Some(3)));
    }

    #[test]
    fn only_operands_above_the_floor_find_nothing() {
        let mt = memtable();
        write(&mt, b"k", 3, Kind::Merge);
        write(&mt, b"k", 4, Kind::Merge);
        assert_eq!(skip(&mt, b"k", u64::MAX, 2), ended(true, None));
    }

    #[test]
    fn a_missing_key_finds_nothing() {
        let mt = memtable();
        assert_eq!(skip(&mt, b"k", u64::MAX, 0), ended(false, None));
        write(&mt, b"j", 1, Kind::Put);
        assert_eq!(skip(&mt, b"k", u64::MAX, 0), ended(false, None));
    }

    #[test]
    fn neighbouring_keys_never_leak_in() {
        let mt = memtable();
        write(&mt, b"j", 1, Kind::Put);
        write(&mt, b"k", 3, Kind::Merge);
        write(&mt, b"k", 4, Kind::Merge);
        write(&mt, b"ka", 5, Kind::Put);
        write(&mt, b"l", 6, Kind::Delete);
        assert_eq!(skip(&mt, b"k", u64::MAX, 2), ended(true, None));
        assert_eq!(skip(&mt, b"k", u64::MAX, 0), ended(true, None));
    }

    #[test]
    fn entries_above_the_lookup_snapshot_are_ignored() {
        let mt = memtable();
        write(&mt, b"k", 1, Kind::Put);
        write(&mt, b"k", 2, Kind::Merge);
        write(&mt, b"k", 9, Kind::Put);
        assert_eq!(skip(&mt, b"k", 5, 0), ended(true, Some(1)));
        assert_eq!(skip(&mt, b"k", u64::MAX, 0), ended(false, Some(9)));
    }

    #[test]
    fn a_skip_reports_whether_it_passed_an_operand() {
        let mt = memtable();
        write(&mt, b"k", 1, Kind::Put);
        write(&mt, b"k", 2, Kind::Merge);
        // The operand at 2 is above the floor at 1 and is passed over.
        assert_eq!(skip(&mt, b"k", u64::MAX, 1), ended(true, Some(1)));
        // At the floor it is the stop itself, so nothing was passed.
        assert_eq!(skip(&mt, b"k", u64::MAX, 2), ended(false, Some(2)));
        // Below the operand's snapshot only the put is visible.
        assert_eq!(skip(&mt, b"k", 1, 0), ended(false, Some(1)));
    }

    const KEYS: [&[u8]; 3] = [b"a", b"ab", b"b"];

    /// Writes as `(key index, kind)` in sequence order from 1, a floor and
    /// a lookup snapshot, both drawn from just past the last sequence, the
    /// snapshot sometimes unbounded.
    fn scenario() -> impl Strategy<Value = (Vec<(usize, Kind)>, u64, u64)> {
        let kind = prop_oneof![Just(Kind::Put), Just(Kind::Delete), Just(Kind::Merge)];
        proptest::collection::vec((0..KEYS.len(), kind), 0..48).prop_flat_map(|writes| {
            let past_last = writes.len() as u64 + 1;
            (
                Just(writes),
                0..=past_last,
                prop_oneof![0..=past_last, Just(u64::MAX)],
            )
        })
    }

    proptest! {
        #[test]
        fn skip_merges_above_matches_a_linear_model(
            (writes, floor, snapshot_seq) in scenario(),
        ) {
            let mt = memtable();
            for (index, &(key, kind)) in writes.iter().enumerate() {
                write(&mt, KEYS[key], index as u64 + 1, kind);
            }
            for (key, name) in KEYS.iter().enumerate() {
                // Sequences rise with the index, so the reversed log is
                // the key's entries newest first.
                let mut expected = ended(false, None);
                let visible = writes
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(index, &(k, kind))| (k, index as u64 + 1, kind))
                    .filter(|&(k, seq, _)| k == key && seq <= snapshot_seq);
                for (_, seq, kind) in visible {
                    if seq <= floor || kind != Kind::Merge {
                        expected.stop = Some(seq);
                        expected.stop_type = kind.value_type();
                        break;
                    }
                    expected.passed = true;
                }
                prop_assert_eq!(skip_typed(&mt, name, snapshot_seq, floor), expected);
            }
        }
    }
}
