use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::checks::{Check, CheckDescription, CheckVerdict};
use crate::optimization::driver::TEST_CLOCK_OFFSET;
use crate::optimization::proposer::{Proposal, ProposalInput, Proposer, ScriptedProposer};
use crate::runner::embedded::observe::ObservationHook;
use crate::runner::liveness::marker_poll;
use crate::runner::{
    read_progress, request_cancel, trial_id_for, Capture, DocumentRecorder, InFlight, Isolation,
    LiveSnapshot, Progress, Recorder, RunOptions, ScriptedExecutor, StageEvidence, TrialEvidence,
    TrialExecutor, TrialLocator, TrialSpec, CANCEL_MARKER,
};
use crate::{OutcomeKind, TrialCompletion, TrialIdentity, TrialRecord, VerdictDraft};
use gents_documents::EvalSplit;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::STALL_CEILING;

pub(crate) fn passed_evidence() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

fn default_trial_locator() -> TrialLocator {
    TrialLocator {
        trial_node_did: "did:key:trial".into(),
        session_id: "session".into(),
        home_hint: None,
    }
}

pub(crate) async fn progress_when<T>(
    run_dir: &Path,
    what: &str,
    found: impl Fn(&Progress) -> Option<T>,
) -> T {
    let mut reread = tokio::time::interval(marker_poll(&RunOptions::default()));
    let mut last = None;
    let mut changed = std::time::Instant::now();
    loop {
        reread.tick().await;
        let progress = read_progress(run_dir);
        if let Some(answer) = progress.as_ref().and_then(&found) {
            return answer;
        }
        if progress != last {
            last = progress;
            changed = std::time::Instant::now();
        } else if changed.elapsed() >= STALL_CEILING {
            panic!("{what}: the progress stayed {last:?} for {STALL_CEILING:?}");
        }
    }
}

pub(crate) struct FeedbackCheck {
    pub(crate) feedback: &'static str,
}

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
            feedback: Some(self.feedback.into()),
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

pub(crate) struct ErroringProposer;

#[async_trait::async_trait]
impl Proposer for ErroringProposer {
    async fn propose(&self, _input: ProposalInput) -> Result<Proposal> {
        Err(anyhow!("proposer unavailable"))
    }
}

pub(crate) struct CancellingProposer {
    pub(crate) inner: ScriptedProposer,
    pub(crate) at: u32,
    pub(crate) token: CancellationToken,
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

pub(crate) struct DeadlineProposer {
    pub(crate) inner: ScriptedProposer,
    pub(crate) deadline: u64,
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

pub(crate) struct CancellingExecutor<'a> {
    pub(crate) inner: &'a ScriptedExecutor,
    pub(crate) run_id: String,
    pub(crate) token: CancellationToken,
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

pub(crate) struct CancelsFirstValidationRun<'a> {
    pub(crate) inner: &'a ScriptedExecutor,
    pub(crate) marked: AtomicBool,
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

pub(crate) struct RevokeOnObservation {
    pub(crate) owner: ConfigAccess,
    pub(crate) reader: identity::Did,
    pub(crate) doc: String,
    pub(crate) done: AtomicBool,
}

#[async_trait::async_trait]
impl ObservationHook for RevokeOnObservation {
    async fn on_state(&self, _state: &str, _session_id: Option<&str>) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        self.owner
            .delete_actor_relationship("AgentRequest", &self.doc, "reader", &self.reader)
            .await
            .unwrap();
        self.owner
            .write(
                "eval.test.signal_after_revoke",
                &format!(
                    "mutation {{ update_AgentRequest(docID: \"{}\", input: {{lifecycle_state: \"processing\"}}) {{ _docID }} }}",
                    escape_graphql_string(&self.doc)
                ),
            )
            .await
            .unwrap();
    }
}

pub(crate) struct DeadlineHook {
    pub(crate) states: AtomicUsize,
    pub(crate) interrupts: AtomicUsize,
}

#[async_trait::async_trait]
impl ObservationHook for DeadlineHook {
    async fn on_state(&self, _state: &str, _session_id: Option<&str>) {
        self.states.fetch_add(1, Ordering::SeqCst);
    }

    async fn on_interrupt_requested(&self) {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) struct BudgetExecutor {
    pub(crate) started:
        tokio::sync::mpsc::UnboundedSender<(String, tokio::sync::oneshot::Sender<()>)>,
    pub(crate) active: AtomicUsize,
    pub(crate) peak: AtomicUsize,
}

#[async_trait::async_trait]
impl TrialExecutor for BudgetExecutor {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let (send, receive) = tokio::sync::oneshot::channel();
        self.started.send((spec.trial_id.clone(), send)).unwrap();
        receive.await.unwrap();
        self.active.fetch_sub(1, Ordering::SeqCst);
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct CancelOnFirstExecute {
    pub(crate) run: CancellationToken,
}

#[async_trait::async_trait]
impl TrialExecutor for CancelOnFirstExecute {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        self.run.cancel();
        cancel.cancelled().await;
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct MarkOnFirstExecute {
    pub(crate) inner: ScriptedExecutor,
    pub(crate) runs_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) marked: AtomicBool,
}

#[async_trait::async_trait]
impl TrialExecutor for MarkOnFirstExecute {
    fn isolation(&self) -> Isolation {
        self.inner.isolation()
    }

    fn wants_script_key(&self) -> bool {
        self.inner.wants_script_key()
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        self.inner.provision(spec).await
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        if !self.marked.swap(true, Ordering::SeqCst) {
            request_cancel(&self.runs_dir, &self.run_id).unwrap();
        }
        self.inner.execute(spec, cancel).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        self.inner.recollect(at, captures).await
    }
}

pub(crate) struct MarkAfterComplete<'a> {
    pub(crate) inner: DocumentRecorder<'a>,
    pub(crate) runs_dir: PathBuf,
}

#[async_trait::async_trait]
impl Recorder for MarkAfterComplete<'_> {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()> {
        self.inner.create_trial(owner, identity).await
    }

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()> {
        self.inner.append_verdict(owner, split, draft).await
    }

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()> {
        self.inner
            .complete_trial(owner, trial_id, completion)
            .await?;
        request_cancel(&self.runs_dir, "run-late").map(|_| ())
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        self.inner.load_trials(owner, run_id).await
    }
}

pub(crate) struct MarksThenWaits {
    pub(crate) runs_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) interrupted: AtomicBool,
}

#[async_trait::async_trait]
impl TrialExecutor for MarksThenWaits {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        request_cancel(&self.runs_dir, &self.run_id).unwrap();
        if tokio::time::timeout(STALL_CEILING, cancel.cancelled())
            .await
            .is_ok()
        {
            self.interrupted.store(true, Ordering::SeqCst);
        }
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct DrainsSlowly {
    pub(crate) runs_dir: PathBuf,
    pub(crate) run_id: String,
    pub(crate) seen: Mutex<Vec<Option<String>>>,
}

#[async_trait::async_trait]
impl TrialExecutor for DrainsSlowly {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        request_cancel(&self.runs_dir, &self.run_id).unwrap();
        let run_dir = self.runs_dir.join(&self.run_id);
        if tokio::time::timeout(STALL_CEILING, cancel.cancelled())
            .await
            .is_err()
        {
            panic!(
                "the loop did not see the marker for {STALL_CEILING:?}: {:?}",
                read_progress(&run_dir)
            );
        }
        let first = read_progress(&run_dir)
            .and_then(|progress| progress.holder)
            .map(|holder| holder.written_at);
        let second = match &first {
            Some(first) => Some(
                progress_when(&run_dir, "the heartbeat refreshes the holder", |progress| {
                    progress
                        .holder
                        .as_ref()
                        .map(|holder| holder.written_at.clone())
                        .filter(|written_at| written_at > first)
                })
                .await,
            ),
            None => None,
        };
        self.seen.lock().unwrap().extend([first, second]);
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct WatchesHeartbeat {
    pub(crate) run_dir: PathBuf,
    pub(crate) seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl TrialExecutor for WatchesHeartbeat {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, _spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        let written_at = || {
            read_progress(&self.run_dir)
                .and_then(|progress| progress.slots.values().next().cloned())
                .map(|slot| slot.written_at)
                .unwrap_or_default()
        };
        let first = written_at();
        let second = progress_when(
            &self.run_dir,
            "the heartbeat refreshes the slot",
            |progress| {
                progress
                    .slots
                    .values()
                    .next()
                    .map(|slot| slot.written_at.clone())
                    .filter(|written_at| *written_at > first)
            },
        )
        .await;
        self.seen.lock().unwrap().extend([first, second]);
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct StageReporting {
    pub(crate) run_dir: PathBuf,
    pub(crate) seen: Mutex<Vec<Option<Progress>>>,
}

#[async_trait::async_trait]
impl TrialExecutor for StageReporting {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        spec.progress.stage_started("check");
        let during = read_progress(&self.run_dir);
        spec.progress.stage_ended("check");
        self.seen.lock().unwrap().push(during);
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct ReportsLive {
    pub(crate) run_dir: PathBuf,
    pub(crate) seen: Mutex<Option<InFlight>>,
    pub(crate) first: LiveSnapshot,
    pub(crate) second: LiveSnapshot,
}

#[async_trait::async_trait]
impl TrialExecutor for ReportsLive {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        assert!(spec.progress.attached(), "the loop watches every trial");
        spec.progress.live(self.first.clone());
        *self.seen.lock().unwrap() = read_progress(&self.run_dir)
            .and_then(|progress| progress.slots.values().next().cloned());
        spec.progress.live(self.second.clone());
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

#[derive(Default)]
pub(crate) struct SettlesOnCancel {
    pub(crate) settled: AtomicU32,
}

#[async_trait::async_trait]
impl TrialExecutor for SettlesOnCancel {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        cancel.cancelled().await;
        self.settled.fetch_add(1, Ordering::SeqCst);
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct DiscardingExecutor {
    pub(crate) inner: ScriptedExecutor,
    pub(crate) discarded: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl TrialExecutor for DiscardingExecutor {
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
        self.discarded
            .lock()
            .expect("the discard log")
            .push(trial_id.to_owned());
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        self.inner.execute(spec, cancel).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        self.inner.recollect(at, captures).await
    }
}

pub(crate) struct FaultingRecorder<'a> {
    inner: DocumentRecorder<'a>,
    fail_on: Option<(&'static str, u32)>,
    calls: Mutex<Vec<(&'static str, String)>>,
}

impl<'a> FaultingRecorder<'a> {
    pub(crate) fn new(access: &'a ConfigAccess) -> Self {
        Self {
            inner: DocumentRecorder::new(access),
            fail_on: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn failing(mut self, method: &'static str, nth: u32) -> Self {
        self.fail_on = Some((method, nth));
        self
    }

    fn enter(&self, method: &'static str, subject: &str) -> Result<()> {
        let mut calls = self.calls.lock().expect("the recorder call log");
        calls.push((method, subject.to_owned()));
        let nth = calls.iter().filter(|(name, _)| *name == method).count() as u32;
        match self.fail_on {
            Some((failing, at)) if failing == method && at == nth => Err(anyhow!("injected")),
            _ => Ok(()),
        }
    }

    pub(crate) fn log(&self) -> Vec<(&'static str, String)> {
        self.calls.lock().expect("the recorder call log").clone()
    }
}

#[async_trait::async_trait]
impl Recorder for FaultingRecorder<'_> {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()> {
        self.enter("create_trial", &identity.trial_id)?;
        self.inner.create_trial(owner, identity).await
    }

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()> {
        self.enter("append_verdict", &draft.trial_id)?;
        self.inner.append_verdict(owner, split, draft).await
    }

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()> {
        self.enter("complete_trial", trial_id)?;
        self.inner.complete_trial(owner, trial_id, completion).await
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        self.enter("load_trials", run_id)?;
        self.inner.load_trials(owner, run_id).await
    }
}

pub(crate) struct BreakerExecutor {
    pub(crate) gate: CancellationToken,
}

#[async_trait::async_trait]
impl TrialExecutor for BreakerExecutor {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    fn wants_script_key(&self) -> bool {
        true
    }

    async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
        default_trial_locator()
    }

    async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
        let index = spec.script_key.as_ref().map_or(0, |key| key.trial_index);
        if index == 0 {
            return ScriptedExecutor::not_evidence("did:key:x");
        }
        self.gate.cancelled().await;
        passed_evidence()
    }

    async fn recollect(&self, _at: &TrialLocator, _captures: &[Capture]) -> Option<TrialEvidence> {
        None
    }
}

pub(crate) struct GatingRecorder<'a> {
    pub(crate) inner: DocumentRecorder<'a>,
    pub(crate) completions: Mutex<u32>,
    pub(crate) open_after: u32,
    pub(crate) gate: CancellationToken,
}

#[async_trait::async_trait]
impl Recorder for GatingRecorder<'_> {
    async fn create_trial(&self, owner: &str, identity: &TrialIdentity) -> Result<()> {
        self.inner.create_trial(owner, identity).await
    }

    async fn append_verdict(
        &self,
        owner: &str,
        split: EvalSplit,
        draft: &VerdictDraft,
    ) -> Result<()> {
        self.inner.append_verdict(owner, split, draft).await
    }

    async fn complete_trial(
        &self,
        owner: &str,
        trial_id: &str,
        completion: &TrialCompletion,
    ) -> Result<()> {
        self.inner
            .complete_trial(owner, trial_id, completion)
            .await?;
        let mut completions = self.completions.lock().expect("the completion count");
        *completions += 1;
        if *completions >= self.open_after {
            self.gate.cancel();
        }
        Ok(())
    }

    async fn load_trials(&self, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>> {
        self.inner.load_trials(owner, run_id).await
    }
}
