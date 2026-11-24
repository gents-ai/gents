//! The commit-time judgement of a read whose key was written after it: the
//! value rule of DefraLevel and the projected rule of `get_parts`.
//!
//! Both run only for a key whose newest version is newer than the read, so a
//! key nothing touched costs the lookup of its newest version it always cost.
//! On that contended path they read the value the transaction saw, at its
//! pinned snapshot, and compare bytes: no read is hashed when it is made.
//! They run caller code (the merge operator's `touches` and `full_merge`), so
//! a panic there is contained as it is anywhere else in a commit.

use std::io;
use std::ops::ControlFlow;

use super::super::callback::{self, InCommit};
use super::super::internal_key::{VALUE_TYPE_MERGE, VALUE_TYPE_VALUE, write_kind};
use super::super::source_walk::{Above, Source};
use super::super::{
    ConflictKey, LookupKey, Materialize, PointValue, ReadRule, ReadView, RegolithEngine,
    with_key_scratch,
};
use crate::{DbSlice, WriteKind};

/// The newest write above a floor that changes the parts a projected read
/// named.
enum Decisive {
    /// An operand the merge operator says touches one of the parts.
    Operand { seq: u64 },
    /// A put, a delete or a covering range delete, which replaces every part.
    /// `value` is a put's bytes.
    Replaced {
        seq: u64,
        kind: WriteKind,
        value: Option<DbSlice>,
    },
}

impl RegolithEngine {
    /// The write that makes `check` stale, as `(sequence, kind)`, or `None`
    /// when the key's newer versions leave what the read decided on
    /// unchanged. `latest` is the key's newest version, already known to be
    /// newer than the read; `ceil` is the newest sequence the check may see.
    pub(super) fn read_changed(
        &self,
        check: &ConflictKey,
        latest: (u64, WriteKind),
        ceil: u64,
        view: &ReadView,
    ) -> io::Result<Option<(u64, WriteKind)>> {
        if check.presence_only() {
            return Ok(latest.1.is_deletion().then_some(latest));
        }
        match &check.rule {
            ReadRule::Seq => Ok(Some(latest)),
            ReadRule::Value => self.contained(|| {
                // An operand on top is a change whatever it folds to, so the
                // value the read saw is only needed beneath a replacement.
                let old = match latest.1 {
                    WriteKind::Merge => None,
                    _ => self.read_value_at(&check.key, check.observed_seq, view)?,
                };
                self.value_changed(check, latest, old, ceil, view)
            }),
            ReadRule::Parts(parts) => self.contained(|| {
                let old = self.read_value_at(&check.key, check.observed_seq, view)?;
                if old.is_none() {
                    // Existence is part of every decision on parts.
                    return self.value_changed(check, latest, None, ceil, view);
                }
                Ok(
                    match self.parts_decisive(&check.key, check.observed_seq, ceil, parts, view)? {
                        None => None,
                        Some(Decisive::Operand { seq }) => Some((seq, WriteKind::Merge)),
                        Some(Decisive::Replaced { seq, kind, value }) => {
                            let identical =
                                kind == WriteKind::Put && value.as_deref() == old.as_deref();
                            (!identical).then_some((seq, kind))
                        }
                    },
                )
            }),
        }
    }

    /// The value rule: the read is current when the key's newest version is a
    /// put leaving the bytes the read returned (`old`), or a delete or range
    /// delete when it returned nothing. An operand on top is a change.
    fn value_changed(
        &self,
        check: &ConflictKey,
        latest: (u64, WriteKind),
        old: Option<DbSlice>,
        ceil: u64,
        view: &ReadView,
    ) -> io::Result<Option<(u64, WriteKind)>> {
        let identical = match latest.1 {
            WriteKind::Merge => false,
            WriteKind::Delete | WriteKind::RangeDelete => old.is_none(),
            WriteKind::Put => {
                let current = self.read_value_at(&check.key, ceil, view)?;
                old.as_deref() == current.as_deref()
            }
        };
        Ok((!identical).then_some(latest))
    }

    /// What a read of `key` returns at `seq` through `view`: the value, with
    /// a configured merge operator folding the operands first, exactly as a
    /// `get` does.
    fn read_value_at(&self, key: &[u8], seq: u64, view: &ReadView) -> io::Result<Option<DbSlice>> {
        let lk = LookupKey::from_prefixed(key, seq);
        Ok(match self.lookup_loaded(&lk, Materialize::Value, view)? {
            Some(PointValue::Value(value)) => Some(value),
            Some(PointValue::Length(_)) => {
                return Err(io::Error::other(
                    "point lookup produced a length where a value was requested",
                ));
            }
            None => None,
        })
    }

    /// Walk the versions of `key` above `floor` and at or below `ceil`,
    /// newest first, to the first that changes one of `parts`: a put, a
    /// delete or a covering range delete always does, an operand when the
    /// merge operator's `touches` says so. The walk stops there, or at the
    /// floor, so what it reads is bounded by the writes that landed since,
    /// not by the key's history.
    fn parts_decisive(
        &self,
        key: &[u8],
        floor: u64,
        ceil: u64,
        parts: &[u32],
        view: &ReadView,
    ) -> io::Result<Option<Decisive>> {
        let operator = self.merge_operator();
        let lk = LookupKey::from_prefixed(key, ceil);
        let range_delete = |seq: u64| Decisive::Replaced {
            seq,
            kind: WriteKind::RangeDelete,
            value: None,
        };
        let walked = view.walk_newest_first(key, ceil, |source, max_rt_seq| {
            // A covering tombstone hides the entries at or below it, so only
            // the ones above both it and the floor are visited.
            let above = floor.max(max_rt_seq);
            let visit = |seq: u64, value_type: u8, value: DbSlice| match value_type {
                VALUE_TYPE_MERGE
                    if operator.is_none_or(|op| {
                        super::super::merge_call::touches(op, key, &value, parts)
                    }) =>
                {
                    ControlFlow::Break(Decisive::Operand { seq })
                }
                VALUE_TYPE_MERGE => ControlFlow::Continue(()),
                VALUE_TYPE_VALUE => ControlFlow::Break(Decisive::Replaced {
                    seq,
                    kind: WriteKind::Put,
                    value: Some(value),
                }),
                other => ControlFlow::Break(Decisive::Replaced {
                    seq,
                    kind: write_kind(other),
                    value: None,
                }),
            };
            let visited = match source {
                Source::Memtable(mt) => mt.visit_above(&lk, above, visit),
                Source::Table(reader) => {
                    with_key_scratch(|buf| reader.visit_above(&lk, above, buf, &self.cache, visit))?
                }
            };
            Ok(match visited {
                Above::Broke(decisive) => ControlFlow::Break(Some(decisive)),
                // Everything older is hidden or at the floor; a tombstone
                // above the floor is the last replacement there was.
                Above::Floor => {
                    ControlFlow::Break((max_rt_seq > floor).then(|| range_delete(max_rt_seq)))
                }
                Above::Exhausted => ControlFlow::Continue(()),
            })
        })?;
        Ok(match walked {
            ControlFlow::Break(decisive) => decisive,
            ControlFlow::Continue(max_rt_seq) => {
                (max_rt_seq > floor).then(|| range_delete(max_rt_seq))
            }
        })
    }

    /// Run `f`, which calls the caller's merge operator, as part of a commit:
    /// a panic in it is caught, latches the database and fails the commit.
    fn contained<T>(&self, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let _commit = InCommit::enter();
        match callback::contain("MergeOperator", f) {
            Ok(result) => result,
            Err(err) => {
                if let crate::Error::CallbackPanicked { callback, .. } = &err {
                    self.latch_callback_panic(callback);
                }
                Err(err.into_io_error())
            }
        }
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

    const KEYS: [&[u8]; 5] = [b"a", b"b", b"c", b"d", b"e"];
    /// Keys a point write can name; `e` only bounds a range delete.
    const WRITABLE: usize = 4;
    /// Part sets a reader may name.
    const PART_SETS: [&[u32]; 5] = [&[], &[0], &[1], &[0, 2], &[2]];

    /// An operand is one byte, the part it touches; folding keeps the last.
    struct ByPart;

    impl crate::MergeOperator for ByPart {
        fn name(&self) -> &'static str {
            "by-part"
        }

        fn full_merge(&self, _: &[u8], _: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
            operands.last().map(|operand| operand.to_vec())
        }

        fn touches(&self, _: &[u8], operand: &[u8], parts: &[u32]) -> bool {
            parts.contains(&u32::from(operand[0]))
        }
    }

    #[derive(Clone, Debug)]
    enum Op {
        Put(usize, u8),
        Delete(usize),
        Merge(usize, u8),
        DeleteRange(usize, usize),
        Flush,
        Compact,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => (0..WRITABLE, 0u8..2).prop_map(|(k, v)| Op::Put(k, v)),
            2 => (0..WRITABLE).prop_map(Op::Delete),
            5 => (0..WRITABLE, 0u8..3).prop_map(|(k, part)| Op::Merge(k, part)),
            2 => (0..WRITABLE)
                .prop_flat_map(|lo| (Just(lo), lo + 1..KEYS.len()))
                .prop_map(|(lo, hi)| Op::DeleteRange(lo, hi)),
            2 => Just(Op::Flush),
            1 => Just(Op::Compact),
        ]
    }

    /// What the walk must find on `key` above `floor` for `parts`, from the
    /// write log alone: the newest write that replaces the key or touches a
    /// named part.
    fn expected(
        log: &[(u64, Op)],
        key: usize,
        floor: u64,
        parts: &[u32],
    ) -> Option<(u64, WriteKind, Option<Vec<u8>>)> {
        log.iter()
            .rev()
            .take_while(|(seq, _)| *seq > floor)
            .find_map(|(seq, op)| match *op {
                Op::Put(k, v) if k == key => Some((*seq, WriteKind::Put, Some(vec![v]))),
                Op::Delete(k) if k == key => Some((*seq, WriteKind::Delete, None)),
                Op::Merge(k, part) if k == key && parts.contains(&u32::from(part)) => {
                    Some((*seq, WriteKind::Merge, None))
                }
                Op::DeleteRange(lo, hi) if (lo..hi).contains(&key) => {
                    Some((*seq, WriteKind::RangeDelete, None))
                }
                _ => None,
            })
    }

    proptest! {
        // Each case opens a database, so far fewer than the default 256.
        #![proptest_config(ProptestConfig::with_cases(96))]

        /// The projected walk finds exactly the write a log of the key's
        /// history says decides, across memtables, tables and compaction.
        #[test]
        fn the_projected_walk_matches_the_write_log(ops in proptest::collection::vec(op(), 0..40)) {
            let dir = TempDir::new().unwrap();
            let db = Db::open(
                dir.path(),
                Options::default().merge_operator(Some(Arc::new(ByPart))),
            )
            .unwrap();
            let mut log: Vec<(u64, Op)> = Vec::new();
            // A snapshot before the first write and after every write keeps
            // every entry and every range tombstone alive through compaction,
            // so no operand is folded and no tombstone retires (E27): every
            // floor below is one a snapshot protects, as a transaction's are.
            let mut snapshots = vec![db.snapshot()];
            for op in &ops {
                match *op {
                    Op::Put(k, v) => db.put(KEYS[k], &[v]).unwrap(),
                    Op::Delete(k) => db.delete(KEYS[k]).unwrap(),
                    Op::Merge(k, part) => db.merge(KEYS[k], &[part]).unwrap(),
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
            for (key, name) in KEYS.iter().take(WRITABLE).enumerate() {
                let prefixed = prefix_key(DEFAULT_CF_ID, name);
                for floor in 0..=db.latest_sequence() + 1 {
                    for parts in PART_SETS {
                        let got = engine
                            .parts_decisive(&prefixed, floor, u64::MAX, parts, &view)
                            .unwrap()
                            .map(|decisive| match decisive {
                                Decisive::Operand { seq } => (seq, WriteKind::Merge, None),
                                Decisive::Replaced { seq, kind, value } => {
                                    (seq, kind, value.map(|v| v.to_vec()))
                                }
                            });
                        prop_assert_eq!(
                            got,
                            expected(&log, key, floor, parts),
                            "key {:?} floor {} parts {:?}", name, floor, parts
                        );
                    }
                }
            }
        }
    }
}
