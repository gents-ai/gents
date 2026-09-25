//! The recorder seam: the four document operations the loop performs.
//!
//! The loop writes one trial row, its verdicts and its completion, and reads
//! back what a run has already written so a resumed run plans only what it
//! still owes. Naming those four as a trait lets a test crash the loop between
//! any two of them without a fault-injection path in the runtime.
//!
//! A recorder never interprets a failure. Whatever it returns as `Err` is a
//! failure of the record, not of the trial, so the loop propagates it
//! unchanged rather than inventing an outcome for a trial that may well have
//! succeeded.

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::document_config::EvalSplit;
use crate::eval::{
    append_verdict, complete_trial, create_trial, load_trials, TrialCompletion, TrialIdentity,
    TrialRecord, VerdictDraft,
};

#[async_trait::async_trait]
pub trait Recorder: Send + Sync {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()>;

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()>;

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()>;

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>>;
}

/// The recorder every real run uses: the eval documents themselves.
pub struct DocumentRecorder<'a>(pub &'a ConfigAccess);

#[async_trait::async_trait]
impl Recorder for DocumentRecorder<'_> {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()> {
        create_trial(self.0, owner, identity).await
    }

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()> {
        append_verdict(self.0, owner, split, draft).await
    }

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()> {
        complete_trial(self.0, owner, trial_id, completion).await
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        load_trials(self.0, owner, run_id).await
    }
}
