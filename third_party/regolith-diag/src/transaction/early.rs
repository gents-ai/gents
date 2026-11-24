//! Early validation ([`TxnOptions::early_validation`]): a write checks its key
//! against newer versions as it is made, so a doomed transaction stops before
//! it does more work.
//!
//! The check is the commit's own check of a written key (see
//! `engine::commit::write_check`), asked of this one write as if it were the
//! transaction's only. It is advisory in both directions, which is safe
//! because only the commit decides: a key can change after the check, and a
//! write that failed it could have passed had it waited.

use super::{Transaction, TransactionError, TxMode, TxResult, policy};
use crate::engine::commit::EarlyWrite;

impl Transaction {
    /// Fail with the conflict the commit would report for `prefixed` if it is
    /// already lost, when this transaction validates early; otherwise do
    /// nothing, at the cost of one flag test.
    ///
    /// Two kinds of key are left to the commit, because a read of this
    /// transaction can clear what looks like a conflict there: a key it has
    /// read (the read's rule decides, not the write's), and every key once it
    /// has scanned (a write inside a stretch is validated as a read). A put or
    /// merge of a content-addressed key is never validated, early or late.
    #[inline]
    pub(super) fn validate_early(&self, prefixed: &[u8], write: EarlyWrite<'_>) -> TxResult<()> {
        if !self.early_validation {
            return Ok(());
        }
        self.validate_write_now(prefixed, write)
    }

    /// [`Transaction::validate_early`] for a transaction that validates
    /// early.
    fn validate_write_now(&self, prefixed: &[u8], write: EarlyWrite<'_>) -> TxResult<()> {
        if !matches!(self.mode, TxMode::Optimistic)
            || self.scan_runs.get().is_some()
            || self.tracked.get(prefixed).is_some()
        {
            return Ok(());
        }
        if !matches!(write, EarlyWrite::Delete)
            && let Some(classifier) = policy::classifier_for(&self.policy, self.isolation)
            && policy::is_content_addressed(classifier, prefixed)
        {
            return Ok(());
        }
        let conflict = self
            .engine
            .probe_write(
                prefixed,
                write,
                self.snapshot_seq,
                self.isolation.blind_merges_commute(),
            )
            .map_err(|e| TransactionError::Engine(e.into()))?;
        match conflict {
            Some(mut conflict) => {
                conflict.strip_cf_prefix();
                Err(TransactionError::Conflict(conflict))
            }
            None => Ok(()),
        }
    }
}
