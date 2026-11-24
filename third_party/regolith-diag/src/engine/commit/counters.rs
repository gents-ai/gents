//! The counters the pipeline keeps in memory: each counter's last reserved
//! value, carried in the state one slot leaves the next ([`super::After`]).
//!
//! A reservation is decided from the state the slot before it left
//! (`allocate.rs`). Keeping the counter in that state makes the value exact
//! for the slot that reads it, whichever thread decides the slot and however
//! many compute its decision at once: every thread reads the same state, and
//! the decision carries the state the next slot reads. So once a counter has
//! been read, a later reservation finds it here and never reads the device,
//! wherever the counter's record has gone since (a frozen memtable, a table).
//!
//! Invariants this module holds:
//!
//! - A counter here holds the value a read of the key at that slot would
//!   return: every slot that writes a key drops it from the table it carries
//!   on ([`Counters::forget_written`]), and every unit that changes data in
//!   another way (an ingest, emptying the database) carries on an empty
//!   table.
//! - The table is never changed in place: a slot that changes it carries on
//!   a new one, so a decision computed from a state that is later discarded
//!   changes nothing another decision reads.

use std::sync::Arc;

use super::request::Ops;
use crate::WriteBatchOp;

/// Counters a table keeps at the most. A counter past it is not kept: its
/// reservations read the key, which may read the device once the key's
/// record has left the memtables.
// vertexia: a fixed bound with copy-on-write, so a change copies at most
// this many entries; a persistent map would lift the bound if a workload
// keeps more counters busy than this.
pub(super) const MAX_COUNTERS: usize = 64;

/// One counter: its key, column-family prefixed, and its last value.
type Entry = (Arc<[u8]>, u64);

/// The counters as one slot leaves them, sorted by key.
#[derive(Clone, Default)]
pub(crate) struct Counters {
    entries: Option<Arc<[Entry]>>,
}

impl Counters {
    /// The table that keeps nothing.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries
            .as_ref()
            .is_none_or(|entries| entries.is_empty())
    }

    fn entries(&self) -> &[Entry] {
        self.entries.as_deref().unwrap_or(&[])
    }

    fn find(&self, key: &[u8]) -> Result<usize, usize> {
        self.entries()
            .binary_search_by(|(held, _)| held.as_ref().cmp(key))
    }

    /// The counter at `key`, if this table keeps it.
    pub(super) fn get(&self, key: &[u8]) -> Option<u64> {
        self.find(key).ok().map(|at| self.entries()[at].1)
    }

    /// This table with the counter at `key` set to `value`. A key the table
    /// does not keep yet is left out when the table is full.
    pub(super) fn set(&self, key: &[u8], value: u64) -> Self {
        let mut entries = self.entries().to_vec();
        match self.find(key) {
            Ok(at) => entries[at].1 = value,
            Err(_) if entries.len() >= MAX_COUNTERS => return self.clone(),
            Err(at) => entries.insert(at, (Arc::from(key), value)),
        }
        Self {
            entries: Some(entries.into()),
        }
    }

    /// This table without the counters `ops` writes, or `None` when they
    /// write none of them.
    pub(super) fn forget_written(&self, ops: &Ops<'_>) -> Option<Self> {
        if self.is_empty() {
            return None;
        }
        let written = |key: &[u8]| -> bool {
            match ops {
                Ops::None => false,
                Ops::Put { key: put, .. } => *put == key,
                Ops::Batch(_) | Ops::Joined(..) => ops.batch().any(|op| writes(op, key)),
            }
        };
        if !self.entries().iter().any(|(key, _)| written(key)) {
            return None;
        }
        let kept: Vec<Entry> = self
            .entries()
            .iter()
            .filter(|(key, _)| !written(key))
            .cloned()
            .collect();
        Some(Self {
            entries: Some(kept.into()),
        })
    }
}

/// Whether `op` writes `key`.
fn writes(op: &WriteBatchOp, key: &[u8]) -> bool {
    match op {
        WriteBatchOp::Put { key: written, .. }
        | WriteBatchOp::Delete { key: written }
        | WriteBatchOp::Merge { key: written, .. } => written.as_slice() == key,
        WriteBatchOp::DeleteRange { start, end } => start.as_slice() <= key && key < end.as_slice(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(key: &[u8]) -> WriteBatchOp {
        WriteBatchOp::Put {
            key: key.to_vec(),
            value: b"v".to_vec(),
        }
    }

    #[test]
    fn a_counter_is_kept_and_replaced_and_the_old_table_is_untouched() {
        let empty = Counters::empty();
        let one = empty.set(b"b", 5);
        let two = one.set(b"a", 7).set(b"b", 9);
        assert_eq!(empty.get(b"b"), None);
        assert_eq!(one.get(b"b"), Some(5), "a change makes a new table");
        assert_eq!((two.get(b"a"), two.get(b"b")), (Some(7), Some(9)));
    }

    #[test]
    fn a_full_table_keeps_what_it_has_and_leaves_a_new_key_out() {
        let mut table = Counters::empty();
        for i in 0..MAX_COUNTERS as u64 {
            table = table.set(&i.to_be_bytes(), i);
        }
        let full = table.set(b"one more", 1);
        assert_eq!(full.get(b"one more"), None);
        assert_eq!(
            full.set(&3u64.to_be_bytes(), 30).get(&3u64.to_be_bytes()),
            Some(30)
        );
    }

    #[test]
    fn every_kind_of_write_drops_the_counters_it_touches_and_only_those() {
        let table = Counters::empty().set(b"a", 1).set(b"c", 3).set(b"e", 5);
        assert!(table.forget_written(&Ops::None).is_none());
        assert!(
            table
                .forget_written(&Ops::Put {
                    key: b"b",
                    value: b"v"
                })
                .is_none(),
            "nothing it keeps is written: the same table goes on"
        );
        let one = table
            .forget_written(&Ops::Put {
                key: b"c",
                value: b"v",
            })
            .unwrap();
        assert_eq!((one.get(b"a"), one.get(b"c")), (Some(1), None));
        let batch = [
            WriteBatchOp::Delete { key: b"a".to_vec() },
            WriteBatchOp::Merge {
                key: b"e".to_vec(),
                operand: b"x".to_vec(),
            },
            put(b"z"),
        ];
        let after = table.forget_written(&Ops::Batch(&batch)).unwrap();
        assert_eq!(
            (after.get(b"a"), after.get(b"c"), after.get(b"e")),
            (None, Some(3), None)
        );
        let range = [WriteBatchOp::DeleteRange {
            start: b"b".to_vec(),
            end: b"e".to_vec(),
        }];
        let after = table.forget_written(&Ops::Batch(&range)).unwrap();
        assert_eq!(
            (after.get(b"a"), after.get(b"c"), after.get(b"e")),
            (Some(1), None, Some(5)),
            "a range delete drops what it covers, its end excluded"
        );
    }
}
