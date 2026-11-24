//! What a transaction buffers for each key, and how a read and a commit take
//! it.
//!
//! A put, a delete and a merge operand all go into one append-only log, so the
//! order the transaction made them in is the order of the log and nothing
//! else has to remember it. A put or a delete replaces the key outright; an
//! operand lies on whatever the key held when the operand was made. A read
//! folds a key's writes since it was last replaced and applies the operands
//! with the database's merge operator; a commit stores the same fold, so what
//! a transaction read of its own writes is what it commits.

use std::collections::BTreeMap;
use std::io;
use std::ops::ControlFlow;

use crate::DbSlice;
use crate::engine::merge_call::{apply_merge, partial_merge};
use crate::options::MergeOperator;
use crate::txn_buffer::TxnBuffer;

/// One buffered write of a key.
#[derive(Clone, Debug)]
pub(super) enum Write {
    /// Replaces the key's value.
    Put(Vec<u8>),
    /// Removes the key.
    Delete,
    /// An operand for the merge operator, applied to what the key holds at
    /// that point.
    Merge(Vec<u8>),
}

impl Write {
    /// Whether the write replaces the key outright, so nothing the key held
    /// before it shows through.
    pub(super) fn is_terminator(&self) -> bool {
        !matches!(self, Self::Merge(_))
    }
}

/// A key's buffered writes since it was last replaced, folded in the order the
/// transaction made them.
pub(super) struct KeyWrites {
    /// What the newest put or delete leaves (`None` inside: the key is
    /// deleted). `None` when the key was not replaced, so `operands` apply to
    /// what the database holds.
    base: Option<Option<Vec<u8>>>,
    /// The operands made after `base`, oldest first.
    operands: Vec<Vec<u8>>,
}

impl KeyWrites {
    /// Fold one key's writes, newest first, as the buffer yields them.
    pub(super) fn fold(chain: impl IntoIterator<Item = Write>) -> Self {
        let mut base = None;
        let mut operands = Vec::new();
        for write in chain {
            if base.is_some() {
                continue;
            }
            match write {
                Write::Merge(operand) => operands.push(operand),
                Write::Put(value) => base = Some(Some(value)),
                Write::Delete => base = Some(None),
            }
        }
        operands.reverse();
        Self { base, operands }
    }

    /// Whether these writes leave the key as the database has it: operands
    /// with no operator to apply them, over nothing the transaction replaced.
    fn is_inert(&self, merge: Option<&dyn MergeOperator>) -> bool {
        self.base.is_none() && merge.is_none()
    }

    /// Whether applying these writes reads the database: operands that lie on
    /// nothing the transaction replaced, so on what the key holds there.
    pub(super) fn reads_base(&self) -> bool {
        self.base.is_none() && !self.operands.is_empty()
    }

    /// What a read of `key` finds, or `None` when the transaction's writes do
    /// not decide it and the caller reads the database. `Some(None)` is a key
    /// the transaction deleted.
    ///
    /// `committed` reads what the database holds for the key and runs only
    /// when operands apply to it. Operands with no operator configured are
    /// ignored, so such a key reads as it does without them.
    pub(super) fn apply(
        self,
        merge: Option<&dyn MergeOperator>,
        key: &[u8],
        committed: impl FnOnce() -> io::Result<Option<DbSlice>>,
    ) -> io::Result<Option<Option<Vec<u8>>>> {
        let Some(op) = merge.filter(|_| !self.operands.is_empty()) else {
            return Ok(self.base);
        };
        let operands: Vec<&[u8]> = self.operands.iter().map(Vec::as_slice).collect();
        let base = self.base.as_ref().map(Option::as_deref);
        merged(op, key, base, &operands, committed).map(|value| Some(Some(value)))
    }
}

/// What `operands`, oldest first, make of the key they lie on: `base` when the
/// transaction replaced the key (`None` inside: it deleted it), else what
/// `committed` reads from the database. `key` is the buffer's key, which
/// carries the column-family prefix; the operator is handed the caller's.
fn merged(
    op: &dyn MergeOperator,
    key: &[u8],
    base: Option<Option<&[u8]>>,
    operands: &[&[u8]],
    committed: impl FnOnce() -> io::Result<Option<DbSlice>>,
) -> io::Result<Vec<u8>> {
    match base {
        Some(base) => apply_merge(op, key, base, operands),
        None => {
            let committed = committed()?;
            apply_merge(op, key, committed.as_ref().map(DbSlice::as_slice), operands)
        }
    }
}

/// What a read of `key` finds in `writes`, the transaction's own buffer, or
/// `None` when they do not decide it and the caller reads the database.
/// `Some(None)` is a key the transaction deleted.
///
/// The same answer `KeyWrites::apply` gives for the same writes, taken
/// without copying them: the key's chain is walked in place from its newest
/// write back to the one that replaced it, and only the value handed back is
/// copied.
pub(super) fn read_buffered<'op, M, C>(
    writes: &TxnBuffer<Vec<u8>, Write>,
    merge: M,
    key: &[u8],
    committed: C,
) -> io::Result<Option<Option<Vec<u8>>>>
where
    M: FnOnce() -> Option<&'op dyn MergeOperator>,
    C: FnOnce() -> io::Result<Option<DbSlice>>,
{
    let mut operands: Vec<&[u8]> = Vec::new();
    let mut base: Option<Option<&[u8]>> = None;
    writes.walk_chain(key, |write| match write {
        Write::Merge(operand) => {
            operands.push(operand);
            ControlFlow::Continue(())
        }
        Write::Put(value) => {
            base = Some(Some(value.as_slice()));
            ControlFlow::Break(())
        }
        Write::Delete => {
            base = Some(None);
            ControlFlow::Break(())
        }
    });
    // Newest first as walked; the operator folds them oldest first. The
    // operator is looked up only for a key with operands to fold.
    if operands.is_empty() {
        return Ok(base.map(|base| base.map(<[u8]>::to_vec)));
    }
    operands.reverse();
    match merge() {
        Some(op) => merged(op, key, base, &operands, committed).map(|value| Some(Some(value))),
        None => Ok(base.map(|base| base.map(<[u8]>::to_vec))),
    }
}

/// Fold the writes of several keys, `chains` newest first within each key, and
/// order the keys ascending or, for a reverse walk, descending. A key whose
/// writes do not change what the database says is left out.
pub(super) fn fold_by_key(
    mut chains: Vec<(Vec<u8>, Write)>,
    reverse: bool,
    merge: Option<&dyn MergeOperator>,
) -> Vec<(Vec<u8>, KeyWrites)> {
    // Sorts positions, not entries, with the position breaking ties between
    // writes of one key: an unstable sort that still leaves a key's writes
    // newest first, as `fold` needs, and needs no scratch for the entries.
    let mut order: Vec<usize> = (0..chains.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        let keys = if reverse {
            chains[b].0.cmp(&chains[a].0)
        } else {
            chains[a].0.cmp(&chains[b].0)
        };
        keys.then(a.cmp(&b))
    });
    let mut chains = order
        .into_iter()
        .map(|at| std::mem::replace(&mut chains[at], (Vec::new(), Write::Delete)))
        .peekable();
    let mut folded = Vec::new();
    while let Some((key, first)) = chains.next() {
        let rest = std::iter::from_fn(|| chains.next_if(|(next, _)| *next == key));
        let writes = KeyWrites::fold(std::iter::once(first).chain(rest.map(|(_, write)| write)));
        if !writes.is_inert(merge) {
            folded.push((key, writes));
        }
    }
    folded
}

/// What a commit stores: the point writes by key (`None` deletes), and the
/// merge operands that apply on top of them, oldest first.
pub(super) type Settled = (BTreeMap<Vec<u8>, Option<Vec<u8>>>, Vec<(Vec<u8>, Vec<u8>)>);

/// Fold adjacent same-key operands inside one atomic commit when this
/// reduces their combined payload and stays within the value limit.
/// No version of an earlier commit is replaced.
pub(super) fn fold_merges(
    merges: &mut Vec<(Vec<u8>, Vec<u8>)>,
    op: &dyn MergeOperator,
    max_value_size: usize,
) {
    merges.sort_by(|a, b| a.0.cmp(&b.0));
    let mut input = std::mem::take(merges).into_iter();
    merges.reserve(input.len());
    while let Some((key, _)) = input.as_slice().first() {
        let count = input
            .as_slice()
            .iter()
            .take_while(|(next, _)| next == key)
            .count();
        fold_run(&mut input, count, merges, op, max_value_size);
    }
}

fn fold_run(
    input: &mut std::vec::IntoIter<(Vec<u8>, Vec<u8>)>,
    count: usize,
    output: &mut Vec<(Vec<u8>, Vec<u8>)>,
    op: &dyn MergeOperator,
    limit: usize,
) {
    if count == 1 {
        output.push(input.next().expect("the key run is bounded by the input"));
        return;
    }
    let start = output.len();
    fold_run(input, count / 2, output, op, limit);
    let middle = output.len();
    fold_run(input, count - count / 2, output, op, limit);
    // A declined subtree stays intact. Retrying its prefixes would reintroduce
    // an unbalanced fold whose callback work can grow quadratically.
    if middle != start + 1 || output.len() != middle + 1 {
        return;
    }
    let (key, left) = &output[start];
    let right = &output[middle].1;
    if left.len() <= limit
        && right.len() <= limit
        && let Some(combined) = partial_merge(op, key, left, right)
        && combined.len() < left.len().saturating_add(right.len())
        && combined.len() <= limit
    {
        output[start].1 = combined;
        output.pop();
    }
}

/// Split a drained buffer, newest write first, into what a commit stores.
///
/// A put or a delete replaces its key, so an operand older than the newest
/// of either never reaches the engine. Collecting the point writes into a
/// `BTreeMap` with `or_insert` keeps the newest one of a key and restores the
/// key order the engine applies them in.
pub(super) fn settle(drained: Vec<(Vec<u8>, Write)>) -> Settled {
    let mut points = BTreeMap::new();
    let mut operands = Vec::new();
    for (key, write) in drained {
        match write {
            Write::Merge(operand) => {
                if !points.contains_key(&key) {
                    operands.push((key, operand));
                }
            }
            Write::Put(value) => {
                points.entry(key).or_insert(Some(value));
            }
            Write::Delete => {
                points.entry(key).or_insert(None);
            }
        }
    }
    operands.reverse();
    (points, operands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(operand: &[u8]) -> Write {
        Write::Merge(operand.to_vec())
    }

    fn put(value: &[u8]) -> Write {
        Write::Put(value.to_vec())
    }

    /// Appends the operands to the base.
    struct Append;

    impl MergeOperator for Append {
        fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
            let mut out = base.unwrap_or_default().to_vec();
            out.extend(operands.concat());
            Some(out)
        }

        fn name(&self) -> &'static str {
            "append"
        }
    }

    fn applied(writes: KeyWrites, committed: Option<&[u8]>) -> Option<Option<Vec<u8>>> {
        writes
            .apply(Some(&Append), b"k", || {
                Ok(committed.map(|bytes| DbSlice::from(bytes.to_vec())))
            })
            .unwrap()
    }

    #[test]
    fn a_fold_keeps_the_operands_after_the_newest_replacement_oldest_first() {
        let writes = KeyWrites::fold([merge(b"c"), merge(b"b"), put(b"P"), merge(b"a")]);
        assert_eq!(writes.base, Some(Some(b"P".to_vec())));
        assert_eq!(writes.operands, [b"b".to_vec(), b"c".to_vec()]);

        let writes = KeyWrites::fold([merge(b"b"), Write::Delete, put(b"old")]);
        assert_eq!(writes.base, Some(None));
        assert_eq!(writes.operands, [b"b".to_vec()]);

        let writes = KeyWrites::fold([merge(b"b"), merge(b"a")]);
        assert_eq!(writes.base, None);
        assert_eq!(writes.operands, [b"a".to_vec(), b"b".to_vec()]);
    }

    #[test]
    fn operands_apply_to_the_replacement_or_else_to_what_the_database_holds() {
        let over_put = KeyWrites::fold([merge(b"b"), put(b"P")]);
        assert_eq!(applied(over_put, Some(b"db")), Some(Some(b"Pb".to_vec())));

        let over_delete = KeyWrites::fold([merge(b"b"), Write::Delete]);
        assert_eq!(applied(over_delete, Some(b"db")), Some(Some(b"b".to_vec())));

        let over_database = KeyWrites::fold([merge(b"b"), merge(b"a")]);
        assert_eq!(
            applied(over_database, Some(b"db")),
            Some(Some(b"dbab".to_vec()))
        );
        let over_nothing = KeyWrites::fold([merge(b"a")]);
        assert_eq!(applied(over_nothing, None), Some(Some(b"a".to_vec())));
    }

    #[test]
    fn only_operands_over_nothing_replaced_read_the_database() {
        assert!(KeyWrites::fold([merge(b"b"), merge(b"a")]).reads_base());
        assert!(!KeyWrites::fold([merge(b"b"), put(b"P")]).reads_base());
        assert!(!KeyWrites::fold([merge(b"b"), Write::Delete]).reads_base());
        assert!(!KeyWrites::fold([put(b"P")]).reads_base());
        assert!(!KeyWrites::fold([Write::Delete]).reads_base());
    }

    #[test]
    fn writes_without_operands_or_without_an_operator_leave_the_read_to_what_they_replaced() {
        let no_committed = || -> io::Result<Option<DbSlice>> {
            panic!("the database is not read when a replacement decides")
        };
        let writes = KeyWrites::fold([put(b"P")]);
        assert_eq!(
            writes.apply(Some(&Append), b"k", no_committed).unwrap(),
            Some(Some(b"P".to_vec()))
        );
        let writes = KeyWrites::fold([Write::Delete]);
        assert_eq!(
            writes.apply(Some(&Append), b"k", no_committed).unwrap(),
            Some(None)
        );
        let writes = KeyWrites::fold([merge(b"b"), put(b"P")]);
        assert_eq!(
            writes.apply(None, b"k", no_committed).unwrap(),
            Some(Some(b"P".to_vec())),
            "no operator: the operands are ignored"
        );
        let writes = KeyWrites::fold([merge(b"b")]);
        assert_eq!(writes.apply(None, b"k", no_committed).unwrap(), None);
    }

    #[test]
    fn fold_by_key_orders_keys_and_leaves_out_the_inert() {
        let chains = vec![
            (b"b".to_vec(), merge(b"2")),
            (b"a".to_vec(), merge(b"1")),
            (b"b".to_vec(), put(b"B")),
            (b"c".to_vec(), merge(b"3")),
        ];
        let keys = |folded: &[(Vec<u8>, KeyWrites)]| -> Vec<Vec<u8>> {
            folded.iter().map(|(key, _)| key.clone()).collect()
        };

        let forward = fold_by_key(chains.clone(), false, Some(&Append));
        assert_eq!(
            keys(&forward),
            [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        assert_eq!(forward[1].1.base, Some(Some(b"B".to_vec())));
        assert_eq!(forward[1].1.operands, [b"2".to_vec()]);

        let backward = fold_by_key(chains.clone(), true, Some(&Append));
        assert_eq!(
            keys(&backward),
            [b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]
        );

        let without_operator = fold_by_key(chains, false, None);
        assert_eq!(keys(&without_operator), [b"b".to_vec()]);
    }

    #[test]
    fn fold_by_key_keeps_each_keys_writes_in_order_among_many_interleaved_keys() {
        // Newest first, as the buffer yields them, with three keys written in
        // turn on every round: far more entries than a sort handles in one
        // pass of insertion sort, so a key's order is only kept if the sort
        // keeps it.
        let mut chains = Vec::new();
        for round in (0..40u8).rev() {
            for key in [b"k2", b"k0", b"k1"] {
                chains.push((key.to_vec(), merge(&[round])));
            }
        }
        let oldest_first: Vec<Vec<u8>> = (0..40u8).map(|round| vec![round]).collect();

        for reverse in [false, true] {
            let folded = fold_by_key(chains.clone(), reverse, Some(&Append));
            let keys: Vec<&[u8]> = folded.iter().map(|(key, _)| key.as_slice()).collect();
            let want: [&[u8]; 3] = if reverse {
                [b"k2", b"k1", b"k0"]
            } else {
                [b"k0", b"k1", b"k2"]
            };
            assert_eq!(keys, want, "reverse={reverse}");
            for (key, writes) in &folded {
                assert_eq!(writes.operands, oldest_first, "reverse={reverse} {key:?}");
            }
        }
    }

    #[test]
    fn settle_commits_the_newest_replacement_and_the_operands_after_it() {
        let drained = vec![
            (b"k".to_vec(), merge(b"3")),
            (b"j".to_vec(), merge(b"x")),
            (b"k".to_vec(), merge(b"2")),
            (b"k".to_vec(), put(b"P")),
            (b"k".to_vec(), merge(b"1")),
            (b"j".to_vec(), Write::Delete),
            (b"k".to_vec(), put(b"old")),
            (b"d".to_vec(), merge(b"m")),
        ];
        let (points, operands) = settle(drained);
        assert_eq!(
            points.into_iter().collect::<Vec<_>>(),
            [(b"j".to_vec(), None), (b"k".to_vec(), Some(b"P".to_vec())),]
        );
        assert_eq!(
            operands,
            [
                (b"d".to_vec(), b"m".to_vec()),
                (b"k".to_vec(), b"2".to_vec()),
                (b"j".to_vec(), b"x".to_vec()),
                (b"k".to_vec(), b"3".to_vec()),
            ],
            "oldest first: each key's operands keep the order they were made in"
        );
    }
}
