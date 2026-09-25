//! The scripted end-to-end matrix: no model, no network.
//!
//! Each case assembles a job the way `tests/eval_runner_canary.rs` assembles a
//! run — a launching home, an installed definition and inference documents, a
//! fixture pack as the subject — and replaces only the provider, with the
//! runner's `ScriptedExecutor`. Nothing here mocks the driver, the policy or
//! the journal: a failure is a defect in the stack.
//!
//! Scoring is binary by construction. `captured_rows_count` with `min: 1`
//! scores 10000 on a stage whose `findings` capture holds a row and 0 on one
//! that holds none, so a scripted arm's per-case difference is exactly
//! +10000, 0 or -10000 and every expected decision is arithmetic.
//!
//! Resume twins interrupt a job in the middle of its flow and compare the
//! resumed journal with an uninterrupted twin's (ruling R9, T35-3). The driver
//! checks the token before it journals any run, so a token cancelled inside
//! `propose` ([`CancellingProposer`]) stops the pass after that round's train
//! run and before its next run: the validation run for a new candidate, the
//! held-out run after a final structural duplicate. A token cancelled inside a
//! run's first trial ([`CancellingExecutor`]) stops it with the run journaled
//! and undecided: the validation run, the re-run, the held-out run. The runner
//! abandons that trial and plans its slot again at the next attempt; since
//! spec 4b §9 an abandoned attempt spends no infrastructure retry, so a
//! cancelled slot keeps its pair under any `max_infra_retries`. These twins
//! run with `max_infra_retries: 1` and script attempt 2 as attempt 1.
//!
//! One point is out of the token's reach: after a validation run completes
//! and before its re-run is journaled. No seam is called between the two, so
//! a resume's re-run budget check at that point is covered by the driver unit
//! tests `a_check_charges_only_runs_started_before_it` and
//! `a_resumed_round_is_charged_the_tokens_its_twin_was`, not here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::config_client::ConfigAccess;
use crate::document_config::EvalSplit;
use crate::eval::checks::{Check, CheckRegistry, CheckVerdict};
use crate::eval::report::load_run_rows;
use crate::eval::runner::freeze::tests::{Launching, OWNER};
use crate::eval::runner::{
    trial_id_for, Capture, Isolation, RunOptions, ScriptKey, ScriptedExecutor, StageEvidence,
    TrialEvidence, TrialExecutor, TrialLocator, TrialSpec, CANCEL_MARKER,
};
use crate::eval::{load_run, load_trials, load_verdicts, OutcomeKind, TrialUsage};
use crate::optimization::driver::{
    baseline_dir, run_job, spend_so_far, JobOutcome, JobRequest, TEST_CLOCK_OFFSET,
};
use crate::optimization::evidence::decision_evidence;
use crate::optimization::job::{create_job, load_job, Budgets, JobState, JournalEntry};
use crate::optimization::policy::{Decision, InconclusiveReason, Mode, PolicyV2, RejectReason};
use crate::optimization::proposer::{Proposal, ProposalInput, Proposer, ScriptedProposer};
use crate::optimization::subject::{baseline_text, materialize_candidate, materialize_pack};
use crate::Collection;

pub(crate) const VALIDATION_CASES: [&str; 6] =
    ["val-a", "val-b", "val-c", "val-d", "val-e", "val-f"];
pub(crate) const HELD_OUT_CASES: [&str; 6] = ["ho-a", "ho-b", "ho-c", "ho-d", "ho-e", "ho-f"];
const TRAIN_CASES: [&str; 1] = ["train-a"];
const TRIALS_PER_CASE: u32 = 2;
pub(crate) const BASELINE_PROMPT: &str = "Watch the mailbox.\n";
pub(crate) const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

pub(crate) const DEFINITION: &str = "monitor-findings";
/// Five validation cases: `2^-5` is 31250 ppm, past 16666, so nothing can pass.
const SMALL_DEFINITION: &str = "monitor-small";
/// Every check is `feedback_check`, which always says something.
const FEEDBACK_DEFINITION: &str = "monitor-feedback";
const FEEDBACK: &str = "name the collection";

fn case(case_id: &str, split: &str, check: &str) -> Value {
    let params = if check == "captured_rows_count" {
        json!({"name": "findings", "min": 1})
    } else {
        json!({})
    };
    json!({
        "case_id": case_id,
        "split": split,
        "stages": [{
            "stage_id": "check",
            "prompt": "Run the monitor.",
            "deadline_secs": 600,
            "checks": [{"check": check, "params": params, "tier": "acceptance"}],
        }],
    })
}

pub(crate) fn definition(
    definition_id: &str,
    check: &str,
    validation: &[&str],
    version: i64,
) -> Value {
    let mut cases: Vec<Value> = TRAIN_CASES
        .iter()
        .map(|id| case(id, "train", check))
        .collect();
    cases.extend(validation.iter().map(|id| case(id, "validation", check)));
    cases.extend(HELD_OUT_CASES.iter().map(|id| case(id, "held_out", check)));
    json!({
        "definition_id": definition_id,
        "agent_did": OWNER,
        "comparability_version": version,
        "subject": {"kind": "behavior", "inference_slots": ["primary"]},
        "cases": cases,
    })
}

/// The live configuration the job freezes and, in PR 4, promotes into.
fn behavior(display_name: &str) -> (Collection, Value) {
    (
        Collection::AgentBehavior,
        json!({
            "behavior_id": "monitor",
            "agent_did": OWNER,
            "display_name": display_name,
            "context_id": "monitor-context",
            "inference_profile_id": "local",
        }),
    )
}

fn context(prompt: &str) -> (Collection, Value) {
    (
        Collection::AgentContext,
        json!({
            "context_id": "monitor-context",
            "agent_did": OWNER,
            "display_name": "Monitor",
            "system_prompt": prompt,
            "tools_id": "monitor-tools",
        }),
    )
}

/// The live Tools the subject pack declares: a job's baseline is the live
/// configuration, not only its prompt.
fn tools() -> (Collection, Value) {
    (
        Collection::Tools,
        json!({
            "tools_id": "monitor-tools",
            "agent_did": OWNER,
            "display_name": "Monitor tools",
            "host": {"bash": {"mode": "Off"}},
        }),
    )
}

/// A stage whose `findings` capture holds one row: the check passes at 10000.
pub(crate) fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

/// A stage whose `findings` capture holds nothing: `below_min`, scored 0.
pub(crate) fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

/// The same evidence, reporting `tokens` of usage.
fn with_usage(evidence: TrialEvidence, tokens: u64) -> TrialEvidence {
    TrialEvidence::new(
        evidence.locator,
        evidence.stages,
        TrialUsage {
            input_tokens: Some(tokens),
            output_tokens: Some(0),
        },
        evidence.anchor,
    )
}

/// Script one cell's answer for every trial of `cases`, first attempt.
pub(crate) fn script(
    executor: ScriptedExecutor,
    cell: &str,
    cases: &[&str],
    evidence: impl Fn(&str) -> TrialEvidence,
) -> ScriptedExecutor {
    script_attempts(executor, cell, cases, &[1], evidence)
}

/// Script one cell's answer for every trial of `cases`, at each of `attempts`.
fn script_attempts(
    mut executor: ScriptedExecutor,
    cell: &str,
    cases: &[&str],
    attempts: &[u32],
    evidence: impl Fn(&str) -> TrialEvidence,
) -> ScriptedExecutor {
    for case_id in cases {
        for trial_index in 0..TRIALS_PER_CASE {
            for attempt in attempts {
                let key = ScriptKey {
                    cell_label: cell.into(),
                    case_id: (*case_id).into(),
                    trial_index,
                    attempt: *attempt,
                };
                executor = executor.with(key, evidence(case_id));
            }
        }
    }
    executor
}

/// Every cell passes everywhere except where a test overrides it.
pub(crate) fn base_executor() -> ScriptedExecutor {
    ScriptedExecutor::new().with_default(pass())
}

pub(crate) fn budgets(max_case_trials: u64) -> Budgets {
    Budgets {
        max_rounds: 3,
        max_case_trials,
        max_tokens: u64::MAX,
        deadline_unix_secs: None,
    }
}

/// The uncalibrated defaults: `max_rounds: 3`, `max_reruns: 1`,
/// `alpha_ppm: 50000`, so `alpha_effective` is 16666 ppm.
pub(crate) fn policy() -> PolicyV2 {
    PolicyV2::uncalibrated()
}

/// The same text for all three rounds: rounds 2 and 3 are structural
/// duplicates of round 1.
pub(crate) fn repeating_proposer(text: &str) -> ScriptedProposer {
    ScriptedProposer::new(vec![
        (
            text.to_owned(),
            "name the collection the monitor should read".to_owned()
        );
        3
    ])
}

/// Stands in for a proposer whose model is down.
struct ErroringProposer;

#[async_trait::async_trait]
impl Proposer for ErroringProposer {
    async fn propose(&self, _input: ProposalInput) -> Result<Proposal> {
        Err(anyhow::anyhow!("proposer unavailable"))
    }
}

/// Cancels `token` when it is asked for round `at`, and answers anyway. The
/// round's train run has completed by then; the driver journals the proposal
/// and stops before the next run it would journal (ruling R9).
struct CancellingProposer {
    inner: ScriptedProposer,
    at: u32,
    token: CancellationToken,
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

/// Advances the driver's test clock past `deadline` when it is asked for
/// round 1, so the job's deadline passes between round 1's budget check and
/// round 2's without waiting on the wall clock (ruling FW-4). Must run inside
/// a `TEST_CLOCK_OFFSET` scope.
struct DeadlineProposer {
    inner: ScriptedProposer,
    deadline: u64,
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

/// Cancels `token` inside the first trial of `run_id`, after the driver has
/// journaled that run as started: the trial is abandoned, every other slot is
/// skipped, and the run is left open with no decision. The abandoned slot is
/// planned again at attempt 2, which spends no infrastructure retry (spec 4b
/// §9); the twins that use this script attempt 2 as attempt 1.
struct CancellingExecutor<'a> {
    inner: &'a ScriptedExecutor,
    run_id: String,
    token: CancellationToken,
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

/// Writes its run's cancel marker the first time it runs a candidate trial:
/// an operator's `gents eval cancel` on the job's first validation run.
struct CancelsFirstValidationRun<'a> {
    inner: &'a ScriptedExecutor,
    marked: AtomicBool,
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

/// Always passes, always has advice.
struct FeedbackCheck;

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
}

pub(crate) struct Harness {
    launching: Launching,
    jobs_dir: PathBuf,
    pack: PathBuf,
}

impl Harness {
    pub(crate) async fn new() -> Self {
        let launching = Launching::new().await;
        launching
            .install(vec![
                (
                    Collection::EvalDefinition,
                    definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 1),
                ),
                (
                    Collection::EvalDefinition,
                    definition(
                        SMALL_DEFINITION,
                        "captured_rows_count",
                        &VALIDATION_CASES[..5],
                        1,
                    ),
                ),
                (
                    Collection::EvalDefinition,
                    definition(FEEDBACK_DEFINITION, "feedback_check", &VALIDATION_CASES, 1),
                ),
                tools(),
                context(BASELINE_PROMPT),
                behavior("Monitor"),
            ])
            .await;
        let pack = launching.pack("subject", "Off");
        // `<launching home>/eval/jobs`, beside `eval/runs` (ruling R8).
        let jobs_dir = launching
            .runs_dir()
            .parent()
            .expect("runs_dir is <scratch>/eval/runs")
            .join("jobs");
        Self {
            launching,
            jobs_dir,
            pack,
        }
    }

    pub(crate) fn access(&self) -> &ConfigAccess {
        &self.launching.access
    }

    pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>) {
        self.launching.install(documents).await;
    }

    pub(crate) async fn delete(&self, collection: Collection, id: &str) {
        self.launching.delete(collection, id).await;
    }

    pub(crate) fn request(
        &self,
        job_id: &str,
        definition_id: &str,
        budgets: Budgets,
    ) -> JobRequest {
        JobRequest {
            job_id: job_id.into(),
            owner: OWNER.into(),
            evaluator_did: self.launching.evaluator_did(),
            behavior_id: "monitor".into(),
            definition_id: definition_id.into(),
            inference_profile_id: "local".into(),
            baseline_pack: self.pack.clone(),
            trials_per_case: TRIALS_PER_CASE,
            budgets,
            max_text_bytes: 32 * 1024,
            seed_base: 1_000,
            jobs_dir: self.jobs_dir.clone(),
            runs_dir: self.launching.runs_dir(),
            source_commit: "matrix".into(),
            source_dirty: false,
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 100,
            deadline_secs: Some(600),
            captures: vec![findings_capture()],
            run_options: RunOptions {
                poll_backoff_base: std::time::Duration::from_millis(1),
                poll_backoff_cap: std::time::Duration::from_millis(2),
            },
        }
    }
}

/// The capture `captured_rows_count` grades. The scripted executor fabricates
/// its rows rather than reading them, so here the list proves only that the
/// job hands it to every run (C1).
pub(crate) fn findings_capture() -> Capture {
    Capture::Documents {
        name: "findings".into(),
        collection: "ExperimentFinding".into(),
        filter: json!({}),
        fields: vec!["finding_id".into()],
    }
}

pub(crate) async fn drive(
    harness: &Harness,
    request: &JobRequest,
    executor: &dyn TrialExecutor,
    proposer: &dyn Proposer,
    registry: &CheckRegistry,
    cancel: CancellationToken,
) -> Result<JobOutcome> {
    run_job(
        harness.access(),
        request,
        executor,
        proposer,
        registry,
        &policy(),
        cancel,
    )
    .await
}

/// Drive a job to its end with the builtin checks.
pub(crate) async fn settle(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    proposer: &dyn Proposer,
) -> JobOutcome {
    drive(
        harness,
        request,
        executor,
        proposer,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

pub(crate) async fn journal(harness: &Harness, job_id: &str) -> Vec<JournalEntry> {
    load_job(harness.access(), OWNER, job_id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no job {job_id}"))
        .journal
}

fn runs_started(journal: &[JournalEntry], split: EvalSplit) -> Vec<String> {
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::RunStarted {
                run_id,
                split: started,
                ..
            } if *started == split => Some(run_id.clone()),
            _ => None,
        })
        .collect()
}

fn decisions(journal: &[JournalEntry]) -> Vec<(Option<u32>, Decision)> {
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::Decided {
                round, decision, ..
            } => Some((*round, *decision)),
            _ => None,
        })
        .collect()
}

fn structural_rejects(journal: &[JournalEntry]) -> usize {
    journal
        .iter()
        .filter(|entry| matches!(entry, JournalEntry::StructuralReject { .. }))
        .count()
}

/// The held-out run starts after every round-scoped entry, and only its
/// confirmation and the finalization follow it: it is requested at finalize.
fn assert_held_out_only_at_finalize(journal: &[JournalEntry]) {
    let held_out = journal
        .iter()
        .position(|entry| {
            matches!(
                entry,
                JournalEntry::RunStarted {
                    split: EvalSplit::HeldOut,
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("no held-out run: {journal:#?}"));
    let last_round_scoped = journal
        .iter()
        .rposition(|entry| {
            matches!(
                entry,
                JournalEntry::RunStarted { round: Some(_), .. }
                    | JournalEntry::Proposed { .. }
                    | JournalEntry::StructuralReject { .. }
                    | JournalEntry::Decided { round: Some(_), .. }
                    | JournalEntry::BudgetExhausted { round: Some(_), .. }
            )
        })
        .unwrap_or_else(|| panic!("no round: {journal:#?}"));
    assert!(held_out > last_round_scoped, "{journal:#?}");
    assert!(
        matches!(
            &journal[held_out + 1..],
            [
                JournalEntry::Decided {
                    round: None,
                    mode: Mode::Confirm,
                    ..
                },
                JournalEntry::Finalized { .. },
            ]
        ),
        "{journal:#?}"
    );
}

/// Drive `request` until `proposer` cancels the pass inside round `at`'s
/// proposal, after that round's train run completed.
async fn cancel_while_proposing(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    at: u32,
) -> JobOutcome {
    let token = CancellationToken::new();
    let proposer = CancellingProposer {
        inner: repeating_proposer(CANDIDATE_PROMPT),
        at,
        token: token.clone(),
    };
    let stopped = drive(
        harness,
        request,
        executor,
        &proposer,
        &CheckRegistry::builtin(),
        token,
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running, "{stopped:#?}");
    stopped
}

/// A harness whose job accepted a candidate and reached `ReadyToPromote`:
/// the baseline fails every validation case and the candidate passes them.
pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{outcome:#?}");
    (harness, request)
}

/// A harness whose job finished without accepting anything: three cases
/// improve and three tie, so p = 8/64 = 125000 ppm.
pub(crate) async fn rejecting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..3], |_| {
        fail()
    });
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote, "{outcome:#?}");
    (harness, request)
}

#[tokio::test]
async fn a_candidate_that_wins_every_case_is_accepted_and_reaches_ready_to_promote() {
    let (harness, request) = accepting_harness("accept").await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Accept), (None, Decision::Accept)],
        "one validation decision, one held-out confirmation: {journal:#?}"
    );
    assert_eq!(
        structural_rejects(&journal),
        2,
        "rounds 2 and 3 repeat round 1's text"
    );
    assert_eq!(
        runs_started(&journal, EvalSplit::HeldOut),
        vec!["accept-held-out".to_owned()],
        "the held-out split is run exactly once"
    );
    assert_held_out_only_at_finalize(&journal);
    let retained = crate::optimization::checkpoint(&journal).expect("round 1 was accepted");
    assert_eq!(
        (retained.round, retained.text.as_str()),
        (1, CANDIDATE_PROMPT)
    );
    // C1: every run the job asked for froze the job's capture list, which is
    // what an executor reads stage evidence from.
    let mut started = 0;
    for split in [EvalSplit::Train, EvalSplit::Validation, EvalSplit::HeldOut] {
        for run_id in runs_started(&journal, split) {
            let frozen =
                crate::eval::runner::freeze::frozen_captures(&request.runs_dir.join(&run_id))
                    .unwrap();
            assert_eq!(frozen, Some(vec![findings_capture()]), "{run_id}");
            started += 1;
        }
    }
    assert_eq!(
        started, 5,
        "three train runs, one validation run, one held-out run: {journal:#?}"
    );
}

/// Ruling F-3: the deadline ends the rounds but never the confirmation. Round
/// 1 is accepted, the deadline passes before round 2, and the held-out run
/// still confirms the checkpoint.
#[tokio::test]
async fn a_deadline_that_passes_after_an_accept_still_runs_the_held_out_confirmation() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    // An hour away: only the proposer's clock advance can pass it.
    let deadline = crate::optimization::driver::now_unix_secs() + 3_600;
    let mut request = harness.request("deadline", DEFINITION, budgets(1_000));
    request.budgets.deadline_unix_secs = Some(deadline);
    let proposer = DeadlineProposer {
        inner: repeating_proposer(CANDIDATE_PROMPT),
        deadline,
    };
    let outcome = TEST_CLOCK_OFFSET
        .scope(
            std::cell::Cell::new(0),
            settle(&harness, &request, &executor, &proposer),
        )
        .await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{journal:#?}");
    let exhausted = journal
        .iter()
        .position(|entry| {
            matches!(entry, JournalEntry::BudgetExhausted { round: Some(2), reason }
                if reason.contains("deadline"))
        })
        .unwrap_or_else(|| panic!("round 2 was not stopped by the deadline: {journal:#?}"));
    let held_out = journal
        .iter()
        .position(|entry| {
            matches!(
                entry,
                JournalEntry::RunStarted {
                    split: EvalSplit::HeldOut,
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("no held-out run: {journal:#?}"));
    assert!(exhausted < held_out, "{journal:#?}");
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Accept), (None, Decision::Accept)]
    );
}

/// C1 (constraint 15): a resume that names another capture list is refused
/// and writes nothing, whatever state the job reached.
#[tokio::test]
async fn a_resume_with_another_capture_list_is_refused() {
    let (harness, request) = accepting_harness("captures-resume").await;
    let before = journal(&harness, &request.job_id).await;
    let mut changed = request.clone();
    changed.captures.clear();
    let error = settle_err(&harness, &changed).await;
    let refusal =
        crate::optimization::driver::job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
    assert!(refusal.0.contains("captures"), "{}", refusal.0);
    assert_eq!(journal(&harness, &request.job_id).await, before);
}

/// A job evaluates the live revision (#1455), not a transfer to it: a pack
/// whose prompt matches but whose other configuration differs is refused at
/// freeze with nothing written, and a pack that differs only where a trial
/// remaps it (its behavior names an inference slot, the live one the profile
/// `local`) freezes.
#[tokio::test]
async fn a_pack_that_is_not_the_live_configuration_is_refused_at_freeze() {
    let harness = Harness::new().await;
    let refusal = |error: anyhow::Error| {
        crate::optimization::driver::job_refused(&error)
            .unwrap_or_else(|| panic!("{error:#}"))
            .0
            .clone()
    };

    let mut request = harness.request("other-tools", DEFINITION, budgets(1_000));
    request.baseline_pack = harness.launching.pack("read-only", "ReadOnly");
    let error = super::freeze_job(harness.access(), &request, &policy())
        .await
        .unwrap_err();
    let reason = refusal(error);
    assert!(
        reason.contains("Tools \"monitor-tools\"") && reason.contains("host"),
        "{reason}"
    );
    assert!(!baseline_dir(&request.jobs_dir, &request.job_id).exists());

    harness.install(vec![behavior("Monitor, renamed")]).await;
    let request = harness.request("other-behavior", DEFINITION, budgets(1_000));
    let error = super::freeze_job(harness.access(), &request, &policy())
        .await
        .unwrap_err();
    let reason = refusal(error);
    assert!(
        reason.contains("AgentBehavior \"monitor\"") && reason.contains("display_name"),
        "{reason}"
    );
    assert!(!baseline_dir(&request.jobs_dir, &request.job_id).exists());

    harness.install(vec![behavior("Monitor")]).await;
    let request = harness.request("equivalent", DEFINITION, budgets(1_000));
    super::freeze_job(harness.access(), &request, &policy())
        .await
        .unwrap();
    assert!(baseline_dir(&request.jobs_dir, &request.job_id).exists());
}

async fn settle_err(harness: &Harness, request: &JobRequest) -> anyhow::Error {
    drive(
        harness,
        request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err()
}

#[tokio::test]
async fn a_candidate_that_wins_only_half_the_cases_is_rejected_for_no_improvement() {
    let (harness, request) = rejecting_harness("no-improvement").await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Reject(RejectReason::NoImprovement))]
    );
    assert!(
        runs_started(&journal, EvalSplit::HeldOut).is_empty(),
        "the held-out split is never touched when no round accepted"
    );
}

#[tokio::test]
async fn one_broken_case_rejects_the_candidate_even_though_the_mean_improves() {
    let harness = Harness::new().await;
    // Five cases improve by +10000; `val-a` regresses by -10000, past the
    // 5000 bp per-case tolerance.
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES[1..], |_| {
        fail()
    });
    executor = script(executor, "candidate", &VALIDATION_CASES[..1], |_| fail());
    let request = harness.request("case-regression", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert_eq!(
        decisions(&journal(&harness, "case-regression").await),
        vec![(Some(1), Decision::Reject(RejectReason::CaseRegression))]
    );
}

#[tokio::test]
async fn a_candidate_that_costs_ten_times_the_tokens_is_rejected_for_cost() {
    let harness = Harness::new().await;
    // The candidate wins every case but spends 1000 tokens a trial against the
    // baseline's 100, past the 25% ceiling.
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| {
        with_usage(fail(), 100)
    });
    executor = script(executor, "candidate", &VALIDATION_CASES, |_| {
        with_usage(pass(), 1_000)
    });
    let request = harness.request("cost-regression", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "cost-regression").await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Reject(RejectReason::CostRegression))]
    );
    assert!(journal.iter().any(|entry| matches!(
        entry,
        JournalEntry::Decided { summary, .. } if !summary.cost_skipped
    )));
}

#[tokio::test]
async fn a_repeated_candidate_is_structurally_rejected_and_costs_no_validation_run() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("structural", DEFINITION, budgets(1_000));
    // The checkpoint's own text materializes to the checkpoint's own digest.
    let outcome = settle(&harness, &request, &executor, &ScriptedProposer::echoing()).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "structural").await;
    assert_eq!(structural_rejects(&journal), 3);
    assert!(journal.iter().any(|entry| matches!(
        entry,
        JournalEntry::StructuralReject { round: 1, diagnostics } if diagnostics.starts_with("duplicate_candidate")
    )));
    assert!(
        runs_started(&journal, EvalSplit::Validation).is_empty(),
        "a structural rejection spends no validation run"
    );
    assert!(decisions(&journal).is_empty(), "and reaches no decision");
}

#[tokio::test]
async fn a_round_that_cannot_be_afforded_is_budget_exhausted_and_spends_nothing() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    // A round costs its train run (1 case x 2 trials x 1 cell = 2) and its
    // validation run (6 x 2 x 2 = 24), and 24 more are reserved for the
    // held-out run: 50 > 25, so round 1 is exhausted before it spends anything.
    let request = harness.request("exhausted", DEFINITION, budgets(25));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::Exhausted);
    let journal = journal(&harness, "exhausted").await;
    assert!(journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: Some(1), .. })));
    assert!(
        decisions(&journal).is_empty(),
        "exhaustion is not a rejection"
    );
    assert!(!journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::RunStarted { .. })));
}

#[tokio::test]
async fn an_inconclusive_round_is_rerun_on_a_new_seed_and_ends_inconclusive() {
    let harness = Harness::new().await;
    // The candidate produces no evidence on two cases: 4 of 12 keys dropped on
    // one side is past both the 20% not-evidence and the 10% asymmetry limits.
    let executor = script(base_executor(), "candidate", &VALIDATION_CASES[..2], |_| {
        ScriptedExecutor::not_evidence("did:key:trial")
    });
    let request = harness.request("inconclusive", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);

    let journal = journal(&harness, "inconclusive").await;
    let decided = journal
        .iter()
        .find_map(|entry| match entry {
            JournalEntry::Decided {
                round: Some(1),
                attempt,
                run_ids,
                decision,
                ..
            } => Some((*attempt, run_ids.clone(), *decision)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{journal:#?}"));
    assert_eq!(decided.0, 1, "one re-run");
    assert_eq!(
        decided.1,
        vec![
            "inconclusive-r1-v0".to_owned(),
            "inconclusive-r1-v1".to_owned()
        ]
    );
    assert_eq!(
        decided.2,
        Decision::Inconclusive(InconclusiveReason::Insufficient)
    );
    let mut seeds = Vec::new();
    for run_id in &decided.1 {
        let run = load_run(harness.access(), OWNER, run_id)
            .await
            .unwrap()
            .unwrap();
        seeds.push(run.origin.seed_base);
    }
    assert_ne!(seeds[0], seeds[1], "the re-run draws a new seed base");

    // Finding F1: the decision read both runs' pairs, added.
    let definition =
        crate::optimization::driver::load_definition(harness.access(), OWNER, DEFINITION)
            .await
            .unwrap();
    let mut runs = Vec::new();
    for run_id in &decided.1 {
        runs.push(
            load_run_rows(harness.access(), OWNER, run_id)
                .await
                .unwrap(),
        );
    }
    let evidence = decision_evidence(&definition, &runs, policy().max_missing_usage_bp);
    let val_c = evidence
        .cases
        .iter()
        .find(|case| case.case_id == "val-c")
        .unwrap();
    assert_eq!(val_c.pairs, 4, "two trials in each of two runs");
}

#[tokio::test]
async fn five_validation_cases_can_never_be_accepted() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..5], |_| {
        fail()
    });
    let request = harness.request("too-few", SMALL_DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert_eq!(
        decisions(&journal(&harness, "too-few").await),
        vec![(
            Some(1),
            Decision::Inconclusive(InconclusiveReason::TooFewCases)
        )]
    );
}

#[tokio::test]
async fn a_checkpoint_that_regresses_on_held_out_fails_the_job() {
    let harness = Harness::new().await;
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    executor = script(executor, "candidate", &HELD_OUT_CASES, |_| fail());
    let request = harness.request("held-out-regression", DEFINITION, budgets(1_000));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(
        outcome.state,
        JobState::Failed {
            reason: "held_out_regression".into()
        }
    );
    let journal = journal(&harness, "held-out-regression").await;
    assert_eq!(
        decisions(&journal),
        vec![
            (Some(1), Decision::Accept),
            (None, Decision::Reject(RejectReason::CaseRegression))
        ]
    );
    assert_eq!(runs_started(&journal, EvalSplit::HeldOut).len(), 1);
    assert_held_out_only_at_finalize(&journal);
}

#[tokio::test]
async fn feedback_reaches_the_proposer_only_from_the_train_run() {
    let harness = Harness::new().await;
    let registry = CheckRegistry::builtin().with(Box::new(FeedbackCheck));
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let request = harness.request("feedback", FEEDBACK_DEFINITION, budgets(1_000));
    drive(
        &harness,
        &request,
        &base_executor(),
        &proposer,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let calls = proposer.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "one proposal per round");
    for call in &calls {
        // One train case, two trials: two rows. A validation run has 24.
        assert_eq!(
            call.feedback.len(),
            TRAIN_CASES.len() * TRIALS_PER_CASE as usize
        );
        assert!(call
            .feedback
            .iter()
            .all(|entry| entry.feedback.as_deref() == Some(FEEDBACK)));
    }
    let validation = load_verdicts(harness.access(), OWNER, "feedback-r1-v0")
        .await
        .unwrap();
    assert!(!validation.is_empty());
    assert!(
        validation.iter().all(|verdict| verdict.feedback.is_none()),
        "the runner never records feedback off the train split"
    );
}

#[tokio::test]
async fn a_baseline_edited_after_the_freeze_fails_the_job_before_any_further_spend() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("drifted", DEFINITION, budgets(1_000));
    let (before, spent) = stopped_after_round_one_train(&harness, &request, &executor).await;

    // An operator edits a closure document the job does not target.
    harness.install(vec![behavior("Monitor, renamed")]).await;
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    let failed = JobState::Failed {
        reason: "baseline_drifted".into(),
    };
    assert_eq!(outcome.state, failed, "{outcome:#?}");
    assert_nothing_after_drift(&harness, "drifted", before, spent, failed).await;
}

#[tokio::test]
async fn a_definition_edited_after_the_freeze_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("definition-drift", DEFINITION, budgets(1_000));
    let (before, spent) = stopped_after_round_one_train(&harness, &request, &executor).await;

    // Finding F5: the definition is not in the closure, so this is not drift.
    harness
        .install(vec![(
            Collection::EvalDefinition,
            definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 2),
        )])
        .await;
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    let failed = JobState::Failed {
        reason: "definition_changed".into(),
    };
    assert_eq!(outcome.state, failed, "{outcome:#?}");
    assert_nothing_after_drift(&harness, "definition-drift", before, spent, failed).await;
}

/// Interrupt `request` mid-round: round 1's train run completed and its
/// proposal is journaled, and the validation run was never started. Returns
/// the journal and the case-trials spent at that point.
async fn stopped_after_round_one_train(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
) -> (Vec<JournalEntry>, u64) {
    cancel_while_proposing(harness, request, executor, 1).await;
    let journal = journal(harness, &request.job_id).await;
    assert!(
        matches!(
            journal.last(),
            Some(JournalEntry::Proposed { round: 1, .. })
        ),
        "{journal:#?}"
    );
    assert!(runs_started(&journal, EvalSplit::Validation).is_empty());
    let spent = spend_so_far(harness.access(), OWNER, &journal)
        .await
        .unwrap()
        .case_trials;
    assert_eq!(
        spent,
        (TRAIN_CASES.len() as u64) * u64::from(TRIALS_PER_CASE),
        "the train run completed before the stop"
    );
    (journal, spent)
}

/// The resume that found the drift journaled only the failure, and spent
/// nothing.
async fn assert_nothing_after_drift(
    harness: &Harness,
    job_id: &str,
    before: Vec<JournalEntry>,
    spent: u64,
    failed: JobState,
) {
    let after = journal(harness, job_id).await;
    let mut expected = before;
    expected.push(JournalEntry::Finalized { state: failed });
    assert_eq!(after, expected);
    assert_eq!(
        spend_so_far(harness.access(), OWNER, &after)
            .await
            .unwrap()
            .case_trials,
        spent,
        "no trial ran after the drift"
    );
}

/// Ruling R9: interrupt a job twice and it reaches the journal of its
/// uninterrupted twin. First a token cancelled while round 1 proposes: the
/// pass stops after round 1's train run, before its validation run. Then a
/// proposer error in round 2, after round 1 was decided and round 2's train
/// run completed.
#[tokio::test]
async fn a_resumed_job_reaches_the_journal_of_its_uninterrupted_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let registry = CheckRegistry::builtin();

    let twin = harness.request("twin-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(
        &harness,
        &twin,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);

    let resumed = harness.request("twin-b", DEFINITION, budgets(1_000));
    cancel_while_proposing(&harness, &resumed, &executor, 1).await;
    let partial = journal(&harness, "twin-b").await;
    assert!(
        matches!(
            partial.last(),
            Some(JournalEntry::Proposed { round: 1, .. })
        ),
        "{partial:#?}"
    );
    assert_eq!(
        runs_started(&partial, EvalSplit::Train),
        vec!["twin-b-r1-train".to_owned()]
    );

    let error = drive(
        &harness,
        &resumed,
        &executor,
        &ErroringProposer,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("proposer unavailable"),
        "{error:#}"
    );
    let partial = journal(&harness, "twin-b").await;
    assert_eq!(decisions(&partial), vec![(Some(1), Decision::Accept)]);
    assert_eq!(
        runs_started(&partial, EvalSplit::Train),
        vec!["twin-b-r1-train".to_owned(), "twin-b-r2-train".to_owned()]
    );

    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    assert_eq!(
        finished.checkpoint.map(|held| held.text),
        uninterrupted.checkpoint.map(|held| held.text)
    );
    let asked: Vec<u32> = proposer
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.round)
        .collect();
    assert_eq!(
        asked,
        vec![2, 3],
        "a journaled proposal is never asked again"
    );

    assert_eq!(
        normalized(journal(&harness, "twin-b").await, "twin-b"),
        normalized(journal(&harness, "twin-a").await, "twin-a"),
    );
}

/// Ruling P-N2, under R9: with no slack in the budget, a job interrupted inside
/// round 1 and again inside round 2 passes and fails each budget check as its
/// uninterrupted twin does.
///
/// Round 1 charges its train run, its validation run and the reserved held-out
/// run: 2 + 24 + 24 = 50. Round 2 adds its own train and validation runs to the
/// 26 round 1 spent: 26 + 2 + 24 + 24 = 76, exactly the budget. Round 2 is a
/// structural duplicate, so round 3's check charges 28 + 50 = 78 and fails.
#[tokio::test]
async fn a_resumed_job_with_no_budget_slack_reaches_the_journal_of_its_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let registry = CheckRegistry::builtin();

    let twin = harness.request("tight-a", DEFINITION, budgets(76));
    let uninterrupted = settle(&harness, &twin, &executor, &proposer).await;
    assert_eq!(
        uninterrupted.state,
        JobState::ReadyToPromote,
        "{uninterrupted:#?}"
    );
    let twin_journal = journal(&harness, "tight-a").await;
    assert_eq!(
        runs_started(&twin_journal, EvalSplit::Train),
        vec!["tight-a-r1-train".to_owned(), "tight-a-r2-train".to_owned()],
        "round 2 fits the budget exactly"
    );
    assert!(twin_journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: Some(3), .. })));

    // Stopped after round 1's train run, before its validation run.
    let resumed = harness.request("tight-b", DEFINITION, budgets(76));
    cancel_while_proposing(&harness, &resumed, &executor, 1).await;
    // Round 1 is already proposed, so this is asked only for round 2, which it
    // cannot answer: the pass stops after round 2's train run is journaled
    // and run.
    let round_one_only = ScriptedProposer::new(vec![(
        CANDIDATE_PROMPT.to_owned(),
        "name the collection the monitor should read".to_owned(),
    )]);
    let error = drive(
        &harness,
        &resumed,
        &executor,
        &round_one_only,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("no answer for round 2"),
        "{error:#}"
    );
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);

    assert_eq!(
        normalized(journal(&harness, "tight-b").await, "tight-b"),
        normalized(twin_journal, "tight-a"),
    );
}

/// Ruling T35-3: a pass cancelled while round 3 proposes a structural
/// duplicate stops after the rounds end, before the held-out run is journaled.
/// Its resume proposes nothing and runs only the held-out run.
#[tokio::test]
async fn a_job_stopped_before_its_held_out_run_resumes_to_the_journal_of_its_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());

    let twin = harness.request("before-held-out-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(
        &harness,
        &twin,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);

    let resumed = harness.request("before-held-out-b", DEFINITION, budgets(1_000));
    cancel_while_proposing(&harness, &resumed, &executor, 3).await;
    let partial = journal(&harness, "before-held-out-b").await;
    assert!(
        matches!(
            partial.last(),
            Some(JournalEntry::StructuralReject { round: 3, .. })
        ),
        "{partial:#?}"
    );
    assert!(runs_started(&partial, EvalSplit::HeldOut).is_empty());

    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    assert!(proposer.calls.lock().unwrap().is_empty());
    let resumed_journal = journal(&harness, "before-held-out-b").await;
    assert_held_out_only_at_finalize(&resumed_journal);
    assert_eq!(
        normalized(resumed_journal, "before-held-out-b"),
        normalized(
            journal(&harness, "before-held-out-a").await,
            "before-held-out-a"
        ),
    );
}

/// The accepting script, with attempt 2 answered as attempt 1.
fn accepting_executor_with_retry() -> ScriptedExecutor {
    script_attempts(
        base_executor(),
        "baseline",
        &VALIDATION_CASES,
        &[1, 2],
        |_| fail(),
    )
}

/// Ruling T35-3: cancel inside the first trial of `<job>-b-<run_suffix>`, a
/// run the driver journaled as started and has not decided, then resume and
/// compare with the uninterrupted `<job>-a`. The runner abandons that trial
/// and leaves the run open; the resume plans the abandoned slot again at
/// attempt 2, which spends no infrastructure retry and which `executor`
/// answers as it answered attempt 1.
async fn assert_twin_after_cancel_inside(
    executor: &ScriptedExecutor,
    job: &str,
    run_suffix: &str,
    expected: JobState,
) {
    let harness = Harness::new().await;
    let twin_id = format!("{job}-a");
    let mut twin = harness.request(&twin_id, DEFINITION, budgets(1_000));
    twin.max_infra_retries = 1;
    let uninterrupted = settle(
        &harness,
        &twin,
        executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(uninterrupted.state, expected, "{uninterrupted:#?}");

    let resumed_id = format!("{job}-b");
    let mut resumed = harness.request(&resumed_id, DEFINITION, budgets(1_000));
    resumed.max_infra_retries = 1;
    let run_id = format!("{resumed_id}-{run_suffix}");
    let token = CancellationToken::new();
    let cancelling = CancellingExecutor {
        inner: executor,
        run_id: run_id.clone(),
        token: token.clone(),
    };
    let stopped = drive(
        &harness,
        &resumed,
        &cancelling,
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        token,
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running, "{stopped:#?}");
    let partial = journal(&harness, &resumed_id).await;
    assert!(
        matches!(
            partial.last(),
            Some(JournalEntry::RunStarted { run_id: started, .. }) if *started == run_id
        ),
        "{partial:#?}"
    );
    let trials = load_trials(harness.access(), OWNER, &run_id).await.unwrap();
    assert_eq!(trials.len(), 1, "one trial launched, then abandoned");
    assert!(trials[0].completion.is_none());

    let finished = settle(
        &harness,
        &resumed,
        executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(finished.state, expected, "{finished:#?}");
    assert_eq!(
        normalized(journal(&harness, &resumed_id).await, &resumed_id),
        normalized(journal(&harness, &twin_id).await, &twin_id),
    );
}

#[tokio::test]
async fn a_job_stopped_inside_its_validation_run_resumes_to_the_journal_of_its_twin() {
    assert_twin_after_cancel_inside(
        &accepting_executor_with_retry(),
        "in-validation",
        "r1-v0",
        JobState::ReadyToPromote,
    )
    .await;
}

/// The re-run boundary: v0 is decided `Insufficient` in memory, the re-run is
/// journaled, and the pass stops inside it. The resume rebuilds `run_ids` from
/// attempt 0 and adds v1's pairs to v0's.
#[tokio::test]
async fn a_job_stopped_inside_its_rerun_resumes_to_the_journal_of_its_twin() {
    let executor = script_attempts(
        base_executor(),
        "candidate",
        &VALIDATION_CASES[..2],
        &[1, 2],
        |_| ScriptedExecutor::not_evidence("did:key:trial"),
    );
    assert_twin_after_cancel_inside(&executor, "in-rerun", "r1-v1", JobState::NothingToPromote)
        .await;
}

#[tokio::test]
async fn a_job_stopped_inside_its_held_out_run_resumes_to_the_journal_of_its_twin() {
    assert_twin_after_cancel_inside(
        &accepting_executor_with_retry(),
        "in-held-out",
        "held-out",
        JobState::ReadyToPromote,
    )
    .await;
}

fn normalized(journal: Vec<JournalEntry>, job_id: &str) -> Vec<String> {
    journal
        .iter()
        .map(|entry| serde_json::to_string(entry).unwrap().replace(job_id, "JOB"))
        .collect()
}

/// Ruling T34-3: a crash between `create_job` and `Frozen` leaves a row with
/// an empty journal, and its resume reaches the uninterrupted twin's journal.
#[tokio::test]
async fn a_job_created_without_frozen_resumes_to_the_journal_of_its_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);

    let twin = harness.request("unfrozen-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(&harness, &twin, &executor, &proposer).await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);
    let origin = load_job(harness.access(), OWNER, "unfrozen-a")
        .await
        .unwrap()
        .unwrap()
        .origin;

    // Everything the freeze wrote before the crash: the baseline copy, then
    // the row. The origin names no job id, so the twin's is this job's.
    let resumed = harness.request("unfrozen-b", DEFINITION, budgets(1_000));
    let source = materialize_pack(&resumed.baseline_pack, OWNER, "monitor").unwrap();
    let copy = materialize_candidate(
        &source,
        OWNER,
        &baseline_text(&source).unwrap(),
        &baseline_dir(&resumed.jobs_dir, "unfrozen-b"),
    )
    .unwrap();
    assert_eq!(copy.digest, origin.subject.pack_digest);
    create_job(harness.access(), "unfrozen-b", OWNER, &origin)
        .await
        .unwrap();
    assert!(journal(&harness, "unfrozen-b").await.is_empty());

    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    let resumed_journal = journal(&harness, "unfrozen-b").await;
    assert_eq!(resumed_journal[0], JournalEntry::Frozen);
    assert_eq!(
        normalized(resumed_journal, "unfrozen-b"),
        normalized(journal(&harness, "unfrozen-a").await, "unfrozen-a"),
    );
}

/// Freeze a job and stop before any run: a pre-cancelled token.
pub(crate) async fn frozen_job(harness: &Harness, job_id: &str) -> JobRequest {
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let first = drive(
        harness,
        &request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        cancelled,
    )
    .await
    .unwrap();
    assert_eq!(first.state, JobState::Running);
    request
}

/// Ruling T34-4: a definition deleted after the freeze has changed, and the
/// job ends instead of erroring on every resume.
#[tokio::test]
async fn a_definition_deleted_after_the_freeze_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let request = frozen_job(&harness, "definition-deleted").await;
    harness
        .launching
        .delete(Collection::EvalDefinition, DEFINITION)
        .await;
    let outcome = settle(
        &harness,
        &request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(
        outcome.state,
        JobState::Failed {
            reason: "definition_changed".into()
        }
    );
}

/// Ruling T34-4: a definition that no longer validates has changed as well.
#[tokio::test]
async fn a_definition_that_no_longer_validates_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let request = frozen_job(&harness, "definition-invalid").await;
    // Desired-state apply refuses an invalid definition, so the stored row is
    // edited underneath it, as a writer that skips validation would.
    harness
        .access()
        .transact("matrix.invalidate_definition", |txn| {
            Box::pin(async move {
                txn.execute(&format!(
                    r#"mutation {{ update_EvalDefinition(filter: {{ definition_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}, input: {{ comparability_version: 0 }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(DEFINITION),
                    crate::graphql::escape_graphql_string(OWNER),
                ))
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
    let error = crate::optimization::driver::load_definition(harness.access(), OWNER, DEFINITION)
        .await
        .unwrap_err();
    assert!(
        crate::eval::runner::freeze_refused(&error).is_some(),
        "{error:#}"
    );
    let outcome = settle(
        &harness,
        &request,
        &base_executor(),
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(
        outcome.state,
        JobState::Failed {
            reason: "definition_changed".into()
        }
    );
}

/// Ruling P-N3 (T35-2): attempt 0 is inconclusive and the re-run does not
/// fit. Round 1 charges train 2 + validation 24 + the reserved held-out 24 =
/// 50, exactly the budget; the re-run would charge 26 + 24 + 24 = 74.
#[tokio::test]
async fn an_inconclusive_round_whose_rerun_is_unaffordable_keeps_its_decision_and_ends_the_rounds()
{
    let harness = Harness::new().await;
    let executor = script(base_executor(), "candidate", &VALIDATION_CASES[..2], |_| {
        ScriptedExecutor::not_evidence("did:key:trial")
    });
    let request = harness.request("inconclusive-tight", DEFINITION, budgets(50));
    let outcome = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(outcome.state, JobState::Exhausted, "{outcome:#?}");
    assert_eq!(outcome.checkpoint, None);

    let journal = journal(&harness, "inconclusive-tight").await;
    let at = journal
        .iter()
        .position(|entry| matches!(entry, JournalEntry::Decided { .. }))
        .unwrap_or_else(|| panic!("{journal:#?}"));
    match &journal[at] {
        JournalEntry::Decided {
            round,
            attempt,
            run_ids,
            decision,
            ..
        } => {
            assert_eq!((*round, *attempt), (Some(1), 0));
            assert_eq!(run_ids, &vec!["inconclusive-tight-r1-v0".to_owned()]);
            assert_eq!(
                *decision,
                Decision::Inconclusive(InconclusiveReason::Insufficient)
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(
            &journal[at + 1],
            JournalEntry::BudgetExhausted { round: None, .. }
        ),
        "{journal:#?}"
    );
    assert_eq!(
        runs_started(&journal, EvalSplit::Validation),
        vec!["inconclusive-tight-r1-v0".to_owned()],
        "the unaffordable re-run never started"
    );
    assert_eq!(decisions(&journal).len(), 1);
    assert!(matches!(
        journal.last(),
        Some(JournalEntry::Finalized {
            state: JobState::Exhausted
        })
    ));
}

/// The cancel marker stops only the run's own child token: a cancelled
/// validation run journals no decision, and the job resumes it.
#[tokio::test]
async fn a_cancelled_validation_run_journals_no_decision_and_the_job_resumes() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("job-cancelled", DEFINITION, budgets(1_000));
    let marking = CancelsFirstValidationRun {
        inner: &executor,
        marked: AtomicBool::new(false),
    };
    let stopped = drive(
        &harness,
        &request,
        &marking,
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running);
    let entries = journal(&harness, "job-cancelled").await;
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            JournalEntry::RunStarted {
                split: EvalSplit::Validation,
                ..
            }
        )),
        "{entries:#?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Decided { .. })),
        "a cancelled run is never decided on: {entries:#?}"
    );

    let resumed = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(resumed.state, JobState::ReadyToPromote, "{resumed:#?}");
}
