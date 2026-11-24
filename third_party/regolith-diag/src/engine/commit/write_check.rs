//! The check of one written key against the versions committed after the
//! transaction's snapshot, shared by the commit's write loop and by early
//! validation, so the two cannot disagree on what a conflicting write is.
//!
//! A write conflicts with any newer version of its key, except a write of
//! exactly the bytes the key already holds (a serial order reaches the same
//! state), and at DefraLevel a merge, which commutes with newer operands and
//! conflicts only with a newer replacement.

use std::io;

use super::super::{ReadView, RegolithEngine};
use super::{Landed, Replaced};
use crate::{Access, Conflict, WriteKind};

/// The verdict on one written key.
pub(super) enum WriteCheck {
    /// Nothing newer than the snapshot landed.
    Clean,
    /// Only operands landed on a blind merge, which commutes with them.
    Commuted,
    /// The write stores what the key already holds.
    Elided,
    /// A newer write decided the race: its sequence and kind.
    Conflict { seq: u64, theirs: WriteKind },
}

/// A write checked before commit.
#[derive(Clone, Copy, Debug)]
pub(crate) enum EarlyWrite<'a> {
    /// A put of these bytes.
    Put(&'a [u8]),
    /// A delete.
    Delete,
    /// A merge operand.
    Merge,
}

impl RegolithEngine {
    /// Check `key`, which the commit writes as `mine`, against `observed_seq`.
    ///
    /// `replaced` is the keys the batch replaces outright, present only when
    /// blind merges commute: a merged key outside it conflicts only with a
    /// newer replacement. `matches_committed` says whether the write stores
    /// what the key's newest version holds, asked only of a key about to
    /// conflict.
    pub(super) fn check_write(
        &self,
        key: &[u8],
        mine: Access,
        observed_seq: u64,
        replaced: Option<&Replaced<'_>>,
        view: &ReadView,
        matches_committed: impl FnOnce(WriteKind) -> io::Result<bool>,
    ) -> io::Result<WriteCheck> {
        // Operands commute, so a key the batch only merges into
        // conflicts only with a replacement newer than the snapshot,
        // and one walk settles that and tells whether operands landed
        // too. That walk names the replacement, which is the write
        // that decided the race even when operands sit on top of it.
        let mut replacement = None;
        if mine == Access::Merge
            && let Some(replaced) = replaced
            && !replaced.contains(key)
        {
            match self.landed_above(key, observed_seq, view)? {
                Landed::Replaced { seq, kind } => replacement = Some((seq, kind)),
                // Operands commute, so a newer merge operand never
                // invalidates a blind merge: the key is accepted despite the
                // newer write.
                Landed::Operands => return Ok(WriteCheck::Commuted),
                Landed::Nothing => return Ok(WriteCheck::Clean),
            }
        }
        let newest = match replacement {
            Some(newest) => Some(newest),
            None => self.latest_version_in_view(key, view)?,
        };
        let Some((seq, theirs)) = newest.filter(|(seq, _)| *seq > observed_seq) else {
            return Ok(WriteCheck::Clean);
        };
        // A blind write of the value the key already holds is not a
        // conflict: the schedule has a serial equivalent reaching the
        // same state. A merge never is one, so a replacement found
        // above needs no check.
        if replacement.is_none() && matches_committed(theirs)? {
            return Ok(WriteCheck::Elided);
        }
        Ok(WriteCheck::Conflict { seq, theirs })
    }

    /// Check one write as it is made, as the commit would check it if it were
    /// the transaction's only write: `None` when it would pass, else the
    /// conflict the commit would name. `key` is prefixed; `blind_merges_commute`
    /// is the validation set's flag.
    ///
    /// Costs one probe of the key's newest version, and a read of its value
    /// for a put or delete that is about to conflict. It holds no lock: a
    /// verdict can be stale by the time the transaction commits, which is
    /// why only the commit's own check decides, and this one only stops a
    /// doomed transaction early.
    pub(crate) fn probe_write(
        &self,
        key: &[u8],
        write: EarlyWrite<'_>,
        observed_seq: u64,
        blind_merges_commute: bool,
    ) -> io::Result<Option<Conflict>> {
        self.ensure_open()?;
        let view = self.view.load();
        let mine = match write {
            EarlyWrite::Put(_) => Access::Put,
            EarlyWrite::Delete => Access::Delete,
            EarlyWrite::Merge => Access::Merge,
        };
        // No other write of this batch is known, so no key is replaced by it.
        let replaced = blind_merges_commute.then(|| Replaced::of(&[]));
        let verdict = self.check_write(
            key,
            mine,
            observed_seq,
            replaced.as_ref(),
            &view,
            |theirs| match write {
                EarlyWrite::Put(value) => {
                    self.committed_equals_after(key, Some(value), &view, theirs)
                }
                EarlyWrite::Delete => self.committed_equals_after(key, None, &view, theirs),
                EarlyWrite::Merge => Ok(false),
            },
        )?;
        Ok(match verdict {
            WriteCheck::Conflict { seq, theirs } => {
                Some(Conflict::new(key.to_vec(), mine, theirs, observed_seq, seq))
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use proptest::prelude::*;
    use tempfile::TempDir;

    use super::*;
    use crate::column_family::{DEFAULT_CF_ID, prefix_key};
    use crate::engine::{CommitOutcome, DurabilityMode, ValidationSet};
    use crate::{Db, Options};

    /// Keeps the newest operand.
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
    enum Setup {
        Put(u8),
        Delete,
        Merge,
        Flush,
    }

    #[derive(Clone, Debug)]
    enum Candidate {
        Put(u8),
        Delete,
        Merge,
    }

    proptest! {
        // Each case opens a database, so far fewer than the default 256.
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Early validation and the commit are one check: a write passes the
        /// probe exactly when a transaction with only that write commits.
        #[test]
        fn the_probe_and_the_commit_agree_on_a_single_write(
            setup in proptest::collection::vec(
                prop_oneof![
                    3 => (0u8..3).prop_map(Setup::Put),
                    2 => Just(Setup::Delete),
                    3 => Just(Setup::Merge),
                    1 => Just(Setup::Flush),
                ],
                0..10,
            ),
            floor_at in 0usize..12,
            blind in any::<bool>(),
            candidate in prop_oneof![
                (0u8..3).prop_map(Candidate::Put),
                Just(Candidate::Delete),
                Just(Candidate::Merge),
            ],
        ) {
            let dir = TempDir::new().unwrap();
            // No worker: a background compaction that folded the key's
            // operands between the probe and the commit would change the
            // kind of its newest version, and the two would name different
            // ones for the same conflict.
            let db = Db::open(
                dir.path(),
                Options::default()
                    .merge_operator(Some(Arc::new(Keep)))
                    .max_background_compactions(0),
            )
            .unwrap();
            let mut seqs = vec![db.latest_sequence()];
            for step in &setup {
                match *step {
                    Setup::Put(v) => db.put(b"k", &[v]).unwrap(),
                    Setup::Delete => db.delete(b"k").unwrap(),
                    Setup::Merge => db.merge(b"k", b"op").unwrap(),
                    Setup::Flush => {
                        db.flush().unwrap();
                        continue;
                    }
                }
                seqs.push(db.latest_sequence());
            }
            let floor = seqs[floor_at % seqs.len()];
            let engine = db.engine();
            let key = prefix_key(DEFAULT_CF_ID, b"k");

            let early = match candidate {
                Candidate::Put(ref v) => EarlyWrite::Put(std::slice::from_ref(v)),
                Candidate::Delete => EarlyWrite::Delete,
                Candidate::Merge => EarlyWrite::Merge,
            };
            let probed = engine.probe_write(&key, early, floor, blind).unwrap();

            let checks = ValidationSet {
                reads: Vec::new(),
                writes_at: Some(floor),
                blind_merges_commute: blind,
                exempt: Vec::new(),
                ranges: Vec::new(),
            };
            let (points, merges) = match candidate {
                Candidate::Put(v) => (BTreeMap::from([(key.clone(), Some(vec![v]))]), Vec::new()),
                Candidate::Delete => (BTreeMap::from([(key.clone(), None)]), Vec::new()),
                Candidate::Merge => (BTreeMap::new(), vec![(key.clone(), b"mine".to_vec())]),
            };
            let outcome = engine
                .commit_optimistic(
                    checks,
                    points,
                    Vec::new(),
                    merges,
                    Vec::new(),
                    DurabilityMode::Eventual,
                )
                .unwrap();
            match (probed, outcome) {
                (None, CommitOutcome::Ok { .. }) => {}
                (Some(early), CommitOutcome::Conflict(late)) => {
                    prop_assert_eq!(
                        (early.mine(), early.theirs(), early.latest_seq()),
                        (late.mine(), late.theirs(), late.latest_seq())
                    );
                }
                (probed, outcome) => prop_assert!(
                    false,
                    "the probe said {probed:?} and the commit {outcome:?}"
                ),
            }
        }
    }
}
