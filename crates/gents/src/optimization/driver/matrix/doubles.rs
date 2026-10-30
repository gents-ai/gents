use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::fixtures::FEEDBACK;
use crate::eval::checks::{Check, CheckDescription, CheckVerdict};
use crate::eval::runner::{
    trial_id_for, Capture, Isolation, ScriptedExecutor, StageEvidence, TrialEvidence,
    TrialExecutor, TrialLocator, TrialSpec, CANCEL_MARKER,
};
use crate::eval::OutcomeKind;
use crate::optimization::driver::TEST_CLOCK_OFFSET;
use crate::optimization::proposer::{Proposal, ProposalInput, Proposer, ScriptedProposer};

pub(super) struct ErroringProposer;

#[async_trait::async_trait]
impl Proposer for ErroringProposer {
    async fn propose(&self, _input: ProposalInput) -> Result<Proposal> {
        Err(anyhow::anyhow!("proposer unavailable"))
    }
}

pub(super) struct CancellingProposer {
    pub(super) inner: ScriptedProposer,
    pub(super) at: u32,
    pub(super) token: CancellationToken,
}

#[async_trait::async_trait]
impl Proposer for CancellingProposer {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal> {
        if input.round == self.at {
            self.token.cancel();
        }
        self.inner.propose(input).await
    }
}

pub(super) struct DeadlineProposer {
    pub(super) inner: ScriptedProposer,
    pub(super) deadline: u64,
}

#[async_trait::async_trait]
impl Proposer for DeadlineProposer {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal> {
        if input.round == 1 {
            let behind = self
                .deadline
                .saturating_sub(crate::optimization::driver::now_unix_secs());
            TEST_CLOCK_OFFSET.with(|offset| offset.set(offset.get() + behind + 1));
            assert!(crate::optimization::driver::now_unix_secs() > self.deadline);
        }
        self.inner.propose(input).await
    }
}

pub(super) struct CancellingExecutor<'a> {
    pub(super) inner: &'a ScriptedExecutor,
    pub(super) run_id: String,
    pub(super) token: CancellationToken,
}

#[async_trait::async_trait]
impl TrialExecutor for CancellingExecutor<'_> {
    fn isolation(&self) -> Isolation {
        self.inner.isolation()
    }

    fn wants_script_key(&self) -> bool {
        self.inner.wants_script_key()
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        self.inner.provision(spec).await
    }

    async fn discard(&self, trial_id: &str) {
        self.inner.discard(trial_id).await;
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        let in_run = spec.script_key.as_ref().is_some_and(|key| {
            trial_id_for(
                &self.run_id,
                &key.cell_label,
                &key.case_id,
                key.trial_index,
                key.attempt,
            ) == spec.trial_id
        });
        if in_run {
            self.token.cancel();
        }
        self.inner.execute(spec, cancel).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        self.inner.recollect(at, captures).await
    }
}

pub(super) struct CancelsFirstValidationRun<'a> {
    pub(super) inner: &'a ScriptedExecutor,
    pub(super) marked: AtomicBool,
}

#[async_trait::async_trait]
impl TrialExecutor for CancelsFirstValidationRun<'_> {
    fn isolation(&self) -> Isolation {
        self.inner.isolation()
    }

    fn wants_script_key(&self) -> bool {
        self.inner.wants_script_key()
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        self.inner.provision(spec).await
    }

    async fn discard(&self, trial_id: &str) {
        self.inner.discard(trial_id).await;
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        let candidate = spec
            .script_key
            .as_ref()
            .is_some_and(|key| key.cell_label == "candidate");
        if candidate && !self.marked.swap(true, Ordering::SeqCst) {
            let run_dir = spec
                .trial_dir
                .parent()
                .and_then(Path::parent)
                .expect("a trial home is <run dir>/trials/<trial_id>");
            std::fs::write(run_dir.join(CANCEL_MARKER), b"").unwrap();
        }
        self.inner.execute(spec, cancel).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        self.inner.recollect(at, captures).await
    }
}

pub(super) struct FeedbackCheck;

impl Check for FeedbackCheck {
    fn name(&self) -> &'static str {
        "feedback_check"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, _params: &Value, _stage: &StageEvidence) -> CheckVerdict {
        CheckVerdict {
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            raw: json!({"reason_code": "ok"}),
            feedback: Some(FEEDBACK.into()),
        }
    }

    fn describe(&self) -> CheckDescription {
        CheckDescription {
            name: self.name().into(),
            version: self.version().into(),
            summary: "Always passes with feedback.".into(),
            params_schema: json!({"type": "object", "properties": {}}),
            reads: Vec::new(),
            reason_codes: vec![("ok".into(), "always".into())],
        }
    }
}
