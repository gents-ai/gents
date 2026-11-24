//! Snapshot stripes: what compaction may do to one user key's versions
//! while snapshots are live.
//!
//! The live snapshots cut the sequence line into stripes. An entry at `seq`
//! belongs to the stripe bounded above by the smallest live snapshot at or
//! above `seq`, or to the top stripe, read only at the head, when there is
//! none. A reader sees a stripe whole or not at all: a snapshot reads every
//! stripe up to its own bound and the head reads them all. So only a
//! stripe's newest state is visible to anyone, and nothing beneath it is.
//!
//! [`Stripes::reduce_group`] reduces each stripe of a key on its own and
//! never lets an operation cross a boundary. The user's filter judges the
//! value that ends each stripe, entries beneath it are dropped, and the
//! operands over it fold into one entry. A stripe of operands alone stays
//! operands, since the value they build on is in an older stripe or in a
//! file outside the compaction.

use crate::engine::internal_key::{
    VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, VALUE_TYPE_VALUE, decode_internal_key,
    encode_internal_key,
};
use crate::engine::range_tombstone::RangeTombstoneSet;
use crate::engine::{filter_call, merge_call};
use crate::options::{CompactionDecision, CompactionFilter, MergeOperator};

/// An internal key and its value.
type Entry = (Vec<u8>, Vec<u8>);

/// The reader boundaries and the user hooks one compaction pass reduces
/// each key against.
pub(super) struct Stripes<'a> {
    /// Sequences of the live snapshots, ascending and distinct.
    live: &'a [u64],
    filter: Option<&'a dyn CompactionFilter>,
    merge: Option<&'a dyn MergeOperator>,
    /// The level the pass writes, as the filter sees it.
    level: usize,
}

impl<'a> Stripes<'a> {
    /// Stripes cut at `live`, which must be ascending and distinct.
    pub(super) fn new(
        live: &'a [u64],
        filter: Option<&'a dyn CompactionFilter>,
        merge: Option<&'a dyn MergeOperator>,
        level: usize,
    ) -> Self {
        debug_assert!(live.windows(2).all(|pair| pair[0] < pair[1]));
        Self {
            live,
            filter,
            merge,
            level,
        }
    }

    /// The upper bound of the stripe `seq` falls in: the smallest live
    /// snapshot at or above it, or `u64::MAX` for the top stripe. Two
    /// entries share a stripe exactly when this is the same for both.
    fn top(&self, seq: u64) -> u64 {
        let at_or_above = self.live.partition_point(|&snapshot| snapshot < seq);
        self.live.get(at_or_above).copied().unwrap_or(u64::MAX)
    }

    /// Whether a range tombstone hides the entry `(user_key, seq)` from
    /// every reader that can see it: one covering the key with a greater
    /// sequence that the entry's own stripe can see.
    pub(super) fn shadowed(
        &self,
        tombstones: &RangeTombstoneSet,
        user_key: &[u8],
        seq: u64,
    ) -> bool {
        !tombstones.is_empty() && tombstones.max_covering_seq(user_key, self.top(seq)) > seq
    }

    /// Reduce one user key's versions, newest first, a stripe at a time.
    pub(super) fn reduce_group(&self, mut group: Vec<Entry>) -> Vec<Entry> {
        if group.len() < 2 && self.filter.is_none() {
            return group;
        }
        let mut reduced = Vec::with_capacity(group.len());
        for stripe in group.chunk_by_mut(|a, b| self.top(seq_of(&a.0)) == self.top(seq_of(&b.0))) {
            self.reduce_stripe(stripe, &mut reduced);
        }
        reduced
    }

    /// Reduce one stripe of a key, newest first, appending what survives
    /// to `out`: its newest state, with the filter applied to the value
    /// that ends it and the operands over that value folded into one entry.
    fn reduce_stripe(&self, stripe: &mut [Entry], out: &mut Vec<Entry>) {
        // The newest state is the leading operands and the first value or
        // deletion under them. Whatever is older no reader can see.
        let operands = stripe
            .iter()
            .take_while(|(key, _)| decode_internal_key(key).2 == VALUE_TYPE_MERGE)
            .count();
        let (kept, _invisible) = stripe.split_at_mut(stripe.len().min(operands + 1));
        if let Some(terminator) = kept.get_mut(operands) {
            self.filter_value(terminator);
        }
        match self.merge.and_then(|op| fold(op, kept, operands)) {
            Some(folded) => out.push(folded),
            None => out.extend(kept.iter_mut().map(std::mem::take)),
        }
    }

    /// Apply the user filter to `entry` if it is a value: keep it, change
    /// it, or replace it with a deletion at the same sequence so an older
    /// version deeper in the tree cannot resurface.
    fn filter_value(&self, entry: &mut Entry) {
        let Some(filter) = self.filter else { return };
        let (key, value) = entry;
        let (user_key, seq, value_type) = decode_internal_key(key);
        if value_type != VALUE_TYPE_VALUE {
            return;
        }
        match filter_call::filter_value(filter, self.level, user_key, value) {
            CompactionDecision::Keep => {}
            CompactionDecision::Change(changed) => *value = changed,
            CompactionDecision::Remove => {
                *key = encode_internal_key(user_key, seq, VALUE_TYPE_DELETION);
                value.clear();
            }
        }
    }
}

/// The one entry `merge` folds a stripe's leading `operands` into, along
/// with the value or deletion under them when `kept` ends in one. `None`
/// when there is nothing to fold or the operator declines, in which case
/// the stripe stays as written.
fn fold(merge: &dyn MergeOperator, kept: &[Entry], operands: usize) -> Option<Entry> {
    if operands == 0 {
        return None;
    }
    // The entry takes the newest operand's sequence. `DefraLevel`'s blind
    // merge check is unaffected: a fold yields a value above a
    // transaction's snapshot only when a put or delete landed in that same
    // stripe, which the check already counts as a replacement; an
    // operand-only stripe stays operands; and no fold moves the key's
    // newest sequence, so read validation sees the same newest write.
    let (user_key, newest_seq, _) = decode_internal_key(&kept[0].0);
    // Oldest first, the order both operator methods take.
    let mut oldest_first = kept[..operands]
        .iter()
        .rev()
        .map(|(_, operand)| operand.as_slice());
    match kept.get(operands) {
        Some((key, value)) => {
            let base = match decode_internal_key(key).2 {
                VALUE_TYPE_VALUE => Some(value.as_slice()),
                VALUE_TYPE_DELETION => None,
                _ => return None,
            };
            let operands: Vec<&[u8]> = oldest_first.collect();
            let merged = merge_call::full_merge(merge, user_key, base, &operands)?;
            Some((
                encode_internal_key(user_key, newest_seq, VALUE_TYPE_VALUE),
                merged,
            ))
        }
        None if operands > 1 => {
            let oldest = oldest_first.next()?.to_vec();
            let folded = oldest_first.try_fold(oldest, |folded, next| {
                merge_call::partial_merge(merge, user_key, &folded, next)
            })?;
            Some((
                encode_internal_key(user_key, newest_seq, VALUE_TYPE_MERGE),
                folded,
            ))
        }
        None => None,
    }
}

/// The sequence of an internal key.
fn seq_of(internal_key: &[u8]) -> u64 {
    decode_internal_key(internal_key).1
}

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod hot_counter;
#[cfg(test)]
mod invariant;
#[cfg(test)]
mod late_snapshot;
#[cfg(test)]
mod tests;
