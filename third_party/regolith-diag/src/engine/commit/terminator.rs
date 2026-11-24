//! What landed on a key since a sequence, for the commit check of a blind
//! merge under `IsolationLevel::DefraLevel`.
//!
//! Operands commute, so a newer operand never invalidates a blind merge, but
//! a newer put, delete or covering range delete does. The walk runs while
//! a slot is decided, so it stops at the key's first entry at or below the
//! transaction's snapshot: what it reads is bounded by the writes that landed
//! since, not by the key's history, and it copies nothing.

use std::ops::ControlFlow;

use super::super::internal_key::write_kind;
use super::super::source_walk::Source;
use super::super::{LookupKey, ReadView, RegolithEngine, with_key_scratch};
use crate::WriteKind;

/// What landed on a key above a floor, as a blind merge's commit check sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Landed {
    /// Nothing newer than the floor.
    Nothing,
    /// Only merge operands, which a blind merge commutes with.
    Operands,
    /// A put, a delete or a covering range delete: the newest one above the
    /// floor, with its sequence. Merge operands on top of it do not hide it.
    Replaced { seq: u64, kind: WriteKind },
}

impl RegolithEngine {
    /// What landed on `key` above `floor` in `view`: nothing, only merge
    /// operands, or a replacement (a put, a delete, or a covering range
    /// delete).
    ///
    /// Sources are visited newest first and range-tombstone coverage is
    /// accumulated on the way down, as in `latest_version_in_view`.
    pub(super) fn landed_above(
        &self,
        key: &[u8],
        floor: u64,
        view: &ReadView,
    ) -> std::io::Result<Landed> {
        let snap = u64::MAX;
        let lk = LookupKey::from_prefixed(key, snap);
        // Where a source's skip stopped settles whether a replacement
        // landed. Above the floor and every covering tombstone seen so far,
        // the stop is a value or deletion: a replacement. Otherwise the entry
        // is hidden by a covering tombstone, which is a replacement if it is
        // above the floor, or the entry is at or below the floor, as is
        // everything older.
        let settle = |stop: u64, stop_type: u8, max_rt_seq: u64| {
            if stop > floor.max(max_rt_seq) {
                Some((stop, write_kind(stop_type)))
            } else if max_rt_seq > floor {
                Some((max_rt_seq, WriteKind::RangeDelete))
            } else {
                None
            }
        };

        // With no replacement, no covering tombstone is above the floor, so
        // every skip ran with the floor itself and `passed` is exactly
        // whether an operand above it exists.
        let mut passed = false;
        let walked = view.walk_newest_first(key, snap, |source, max_rt_seq| {
            let skip_floor = floor.max(max_rt_seq);
            let skip = match source {
                Source::Memtable(mt) => mt.skip_merges_above(&lk, skip_floor),
                Source::Table(reader) => with_key_scratch(|buf| {
                    reader.skip_merges_above(&lk, skip_floor, buf, &self.cache)
                })?,
            };
            passed |= skip.passed;
            Ok(match skip.stop {
                Some(stop) => ControlFlow::Break(settle(stop, skip.stop_type, max_rt_seq)),
                None => ControlFlow::Continue(()),
            })
        })?;
        let replaced = match walked {
            ControlFlow::Break(replaced) => replaced,
            ControlFlow::Continue(max_rt_seq) => {
                (max_rt_seq > floor).then_some((max_rt_seq, WriteKind::RangeDelete))
            }
        };
        Ok(match replaced {
            Some((seq, kind)) => Landed::Replaced { seq, kind },
            None if passed => Landed::Operands,
            None => Landed::Nothing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Landed;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};
    use crate::{Db, Options, WriteKind};
    use proptest::prelude::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    const KEYS: [&[u8]; 5] = [b"a", b"b", b"c", b"d", b"e"];

    /// Keeps the newest operand; the walk under test never reads a value.
    struct Keep;

    impl crate::MergeOperator for Keep {
        fn name(&self) -> &'static str {
            "keep"
        }

        fn full_merge(
            &self,
            _key: &[u8],
            _base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            operands.last().map(|operand| operand.to_vec())
        }
    }

    /// Keys a point write can name; `e` only bounds a range delete.
    const WRITABLE: usize = 4;

    /// One step of a workload, over indexes into [`KEYS`].
    #[derive(Clone, Debug)]
    enum Op {
        Put(usize),
        Delete(usize),
        Merge(usize),
        DeleteRange(usize, usize),
        Flush,
        Compact,
    }

    impl Op {
        /// Whether this write replaces `key` outright.
        fn replaces(&self, key: usize) -> bool {
            match *self {
                Self::Put(k) | Self::Delete(k) => k == key,
                Self::DeleteRange(lo, hi) => (lo..hi).contains(&key),
                Self::Merge(_) | Self::Flush | Self::Compact => false,
            }
        }

        /// Whether this write adds a merge operand to `key`.
        fn merges_into(&self, key: usize) -> bool {
            matches!(*self, Self::Merge(k) if k == key)
        }

        /// The kind of write a conflict names this one as.
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

    /// A workload step; a range delete always spans at least one key.
    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (0..WRITABLE).prop_map(Op::Put),
            2 => (0..WRITABLE).prop_map(Op::Delete),
            4 => (0..WRITABLE).prop_map(Op::Merge),
            2 => (0..WRITABLE)
                .prop_flat_map(|lo| (Just(lo), lo + 1..KEYS.len()))
                .prop_map(|(lo, hi)| Op::DeleteRange(lo, hi)),
            2 => Just(Op::Flush),
            1 => Just(Op::Compact),
        ]
    }

    proptest! {
        // Each case opens a database, so far fewer than the default 256.
        #![proptest_config(ProptestConfig::with_cases(96))]
        #[test]
        fn landed_above_matches_the_write_log_across_flushes_and_compactions(
            ops in proptest::collection::vec(op(), 0..40),
        ) {
            let dir = TempDir::new().unwrap();
            // The snapshots below leave each stripe one entry, so the
            // operator is never asked to fold.
            let options = Options::default().merge_operator(Some(Arc::new(Keep)));
            let db = Db::open(dir.path(), options).unwrap();
            let mut log: Vec<(u64, &Op)> = Vec::new();
            // A snapshot before the first write and after every write keeps
            // every entry and every range tombstone alive through compaction:
            // a pass retires a tombstone only with no live snapshot below it
            // (E27), and every floor below is one a snapshot protects, as a
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
                log.push((db.latest_sequence(), op));
                snapshots.push(db.snapshot());
            }

            let view = db.engine().view.load();
            for (key, name) in KEYS.iter().take(WRITABLE).enumerate() {
                let prefixed = prefix_key(DEFAULT_CF_ID, name);
                for floor in 0..=db.latest_sequence() + 1 {
                    let landed = |matches: &dyn Fn(&Op) -> bool| {
                        log.iter().rev().find(|(seq, op)| *seq > floor && matches(op))
                    };
                    let expected = if let Some((seq, op)) = landed(&|op| op.replaces(key)) {
                        Landed::Replaced { seq: *seq, kind: op.kind() }
                    } else if landed(&|op| op.merges_into(key)).is_some() {
                        Landed::Operands
                    } else {
                        Landed::Nothing
                    };
                    let actual = db
                        .engine()
                        .landed_above(&prefixed, floor, &view)
                        .unwrap();
                    prop_assert_eq!(actual, expected, "key {:?} floor {}", name, floor);
                }
            }
        }
    }
}
