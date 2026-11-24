//! The contract check of a put to a content-addressed key: a key that names
//! its bytes never holds different ones.
//!
//! The commit does not validate such a put, so the common path never looks
//! the key up. The caller looks only when a commit landed after the
//! transaction's snapshot, which one atomic load tells, and the bytes are
//! compared only when that commit left the key a value. A key that held
//! different bytes from before the snapshot is not caught: that would cost a
//! read of every such put.

use std::io;

use super::super::{LookupKey, Materialize, PointValue, ReadView, RegolithEngine};
use crate::WriteKind;

impl RegolithEngine {
    /// Whether putting `value` under the content-addressed `key` contradicts
    /// the value a commit made after `floor` left under it.
    ///
    /// Runs while the commit's slot is decided, in ring order, so `view`
    /// holds every commit decided before it and none after.
    /// The caller has already seen that something committed after `floor`.
    pub(super) fn content_mismatch(
        &self,
        key: &[u8],
        value: &[u8],
        floor: u64,
        view: &ReadView,
    ) -> io::Result<bool> {
        let Some((latest_seq, WriteKind::Put)) = self.latest_version_in_view(key, view)? else {
            return Ok(false);
        };
        if latest_seq <= floor {
            return Ok(false);
        }
        let snap = u64::MAX;
        let lk = LookupKey::from_prefixed(key, snap);
        Ok(
            match self.lookup_in_view(key, snap, &lk, Materialize::Value, view)? {
                Some(PointValue::Value(committed)) => *committed != *value,
                Some(PointValue::Length(_)) | None => false,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::super::super::{CommitOutcome, DurabilityMode, EngineOptions, ValidationSet};
    use super::*;
    use crate::WriteBatchOp;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};

    fn key_of(name: &[u8]) -> Vec<u8> {
        prefix_key(DEFAULT_CF_ID, name)
    }

    /// A write that lands after the observed sequence.
    enum Newest {
        Put(&'static [u8]),
        Delete,
        DeleteRange,
        Merge,
    }

    /// Commit a transaction that began before `landed`, putting `mine` under
    /// the exempt key `c`. `flush` moves the landed write out of the memtable.
    fn put_after(landed: Option<Newest>, mine: &[u8], flush: bool) -> io::Result<CommitOutcome> {
        let dir = TempDir::new().unwrap();
        let engine = RegolithEngine::open(dir.path(), EngineOptions::default()).unwrap();
        let observed = engine.snapshot_seq();
        if let Some(landed) = landed {
            let op = match landed {
                Newest::Put(bytes) => WriteBatchOp::Put {
                    key: key_of(b"c"),
                    value: bytes.to_vec(),
                },
                Newest::Delete => WriteBatchOp::Delete { key: key_of(b"c") },
                Newest::DeleteRange => WriteBatchOp::DeleteRange {
                    start: key_of(b"a"),
                    end: key_of(b"d"),
                },
                Newest::Merge => WriteBatchOp::Merge {
                    key: key_of(b"c"),
                    operand: b"op".to_vec(),
                },
            };
            engine
                .apply_batch(vec![op], DurabilityMode::Eventual, false)
                .unwrap();
            if flush {
                engine.flush_active_memtable().unwrap();
            }
        }
        let checks = ValidationSet {
            reads: Vec::new(),
            writes_at: Some(observed),
            blind_merges_commute: false,
            exempt: vec![key_of(b"c")],
            ranges: Vec::new(),
        };
        let puts: BTreeMap<Vec<u8>, Option<Vec<u8>>> =
            BTreeMap::from([(key_of(b"c"), Some(mine.to_vec()))]);
        engine.commit_optimistic(
            checks,
            puts,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            DurabilityMode::Eventual,
        )
    }

    fn mismatched(outcome: io::Result<CommitOutcome>) -> bool {
        matches!(
            outcome.map_err(crate::Error::from),
            Err(crate::Error::ContentMismatch)
        )
    }

    #[test]
    fn a_put_beside_a_commit_of_other_bytes_is_refused() {
        for flush in [false, true] {
            let outcome = put_after(Some(Newest::Put(b"theirs")), b"mine", flush);
            assert!(mismatched(outcome), "flush={flush}");
        }
    }

    #[test]
    fn a_put_beside_a_commit_of_the_same_bytes_commits() {
        for flush in [false, true] {
            let outcome = put_after(Some(Newest::Put(b"same")), b"same", flush).unwrap();
            assert!(matches!(outcome, CommitOutcome::Ok { .. }), "flush={flush}");
        }
    }

    #[test]
    fn a_put_after_nothing_landed_is_not_looked_up() {
        let outcome = put_after(None, b"mine", false).unwrap();
        assert!(matches!(outcome, CommitOutcome::Ok { .. }), "{outcome:?}");
    }

    #[test]
    fn a_newest_version_that_holds_no_value_is_no_mismatch() {
        for flush in [false, true] {
            for landed in [Newest::Delete, Newest::DeleteRange, Newest::Merge] {
                let outcome = put_after(Some(landed), b"mine", flush).unwrap();
                assert!(matches!(outcome, CommitOutcome::Ok { .. }), "flush={flush}");
            }
        }
    }
}
