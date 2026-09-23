//! The driver: the loop that turns a frozen baseline into a checkpoint.
//!
//! Everything the driver decides comes from the job's frozen origin and its
//! journal (finding F2), so a resumed job and an uninterrupted one reach the
//! same state. The driver is the only writer of the job, as the owner DID; it
//! never claims a request, and no runtime reconciles what it writes.

#[cfg(test)]
pub(crate) mod matrix;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::config_client::ConfigAccess;
use crate::document_config::{EvalDefinition, EvalSplit};
use crate::eval::checks::CheckRegistry;
use crate::eval::report::load_run_rows;
use crate::eval::runner::freeze;
use crate::eval::runner::freeze::load_pack;
use crate::eval::runner::{
    freeze_refused, resume, run, Capture, CellRequest, CellSource, RunOptions, RunOutcome,
    RunRequest, TrialExecutor,
};
use crate::eval::{load_run, load_trials, DefinitionRef, RunOrigin, SubjectRef, TrialRecord};
use crate::optimization::evidence::{
    decision_evidence, decision_seed, train_feedback, BASELINE_CELL, CANDIDATE_CELL,
};
use crate::optimization::gate::{structural_gate, text_gate};
use crate::optimization::job::{
    append, append_in_txn, checkpoint, create_job, derive_state, load_job, rounds_used, Budgets,
    Checkpoint, DecisionSummary, JobOrigin, JobRecord, JobState, JournalEntry,
};
use crate::optimization::policy::{
    decide, Decision, DecisionReport, InconclusiveReason, Mode, PolicyV2, RejectReason,
};
use crate::optimization::proposer::{ProposalInput, Proposer, Rejection};
use crate::optimization::subject::{
    baseline_text, materialize_candidate, materialize_pack, MaterializedPack,
};
use crate::optimization::target::{
    baseline_equivalence, capture_closure, closure_digests, current_text, BaselineMismatch,
    Closure, Target, TargetField,
};

/// Everything an operator chose about a job. Read in full only when the job
/// is created; a resume must repeat it, and `run_job` refuses one that does
/// not (finding F2).
#[derive(Clone, Debug)]
pub struct JobRequest {
    pub job_id: String,
    /// The principal the job and its runs belong to, and the only DID that may
    /// promote.
    pub owner: String,
    /// The DID of the home that launched the job, recorded on every run.
    pub evaluator_did: String,
    pub behavior_id: String,
    pub definition_id: String,
    pub inference_profile_id: String,
    /// The operator-supplied baseline subject pack (ruling R5). Copied into the
    /// job's directory at freeze, after its prompt is cross-checked against the
    /// live configuration; the copy is what every run reads.
    pub baseline_pack: PathBuf,
    pub trials_per_case: u32,
    pub budgets: Budgets,
    pub max_text_bytes: usize,
    pub seed_base: i64,
    /// `<launching home>/eval/jobs` (ruling R8). This job owns
    /// `<jobs_dir>/<job_id>/`; M4's `gents optimization rm` removes it.
    pub jobs_dir: PathBuf,
    /// `<launching home>/eval/runs`, handed to the runner unchanged.
    pub runs_dir: PathBuf,
    pub source_commit: String,
    pub source_dirty: bool,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    pub deadline_secs: Option<u64>,
    /// What every run reads out of a finished trial home: the request-level
    /// fallback; stage-level `EvalStage.capture` supersedes it once the runner
    /// reads stage captures. May be empty. Frozen into the origin, so a resume
    /// must repeat it (finding F2).
    pub captures: Vec<Capture>,
    /// Not comparability data: how long the runner backs off between passes.
    pub run_options: RunOptions,
}

/// A job id names the one directory the job owns under `jobs_dir`, so it has
/// to be one ordinary path component, by the rule the runner holds run and
/// cell ids to. Every path below is built from a job id this has accepted;
/// `run_job` and job creation call it before building any. A bad id is a
/// [`JobRefused`]: nothing was written.
pub(crate) fn validate_job_id(job_id: &str) -> Result<()> {
    if job_id.trim().is_empty() {
        return Err(refused(format!("job_id {job_id:?} must not be blank")));
    }
    freeze::directory_name("job_id", job_id).map_err(as_job_refusal)
}

pub fn job_dir(jobs_dir: &Path, job_id: &str) -> PathBuf {
    jobs_dir.join(job_id)
}

pub fn baseline_dir(jobs_dir: &Path, job_id: &str) -> PathBuf {
    job_dir(jobs_dir, job_id).join("baseline")
}

pub fn candidate_dir(jobs_dir: &Path, job_id: &str, round: u32) -> PathBuf {
    job_dir(jobs_dir, job_id)
        .join("rounds")
        .join(round.to_string())
        .join("candidate")
}

/// Where a round's candidate is written before its `Proposed` entry exists
/// (finding F7). Only after the journal names it is it renamed into place.
pub(crate) fn candidate_staging_dir(jobs_dir: &Path, job_id: &str, round: u32) -> PathBuf {
    job_dir(jobs_dir, job_id)
        .join("rounds")
        .join(round.to_string())
        .join("candidate.staging")
}

/// What the job has spent so far, read from the runs its journal names. A
/// report, not the budgeted quantity: the case-trial budget caps planned
/// case-trials ([`case_trials_charged`]), so infrastructure retries and
/// abandoned attempts count here but are never charged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spend {
    /// Trial rows, every attempt included.
    pub case_trials: u64,
    pub tokens: u64,
}

/// One run the driver is about to ask the runner for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunPlan {
    pub run_id: String,
    pub split: EvalSplit,
    pub round: Option<u32>,
    /// `(cell_id, pack directory)`, baseline first.
    pub cells: Vec<(&'static str, PathBuf)>,
    pub seed_base: i64,
}

/// How many cells a run of `split` compares: a train run reads only the
/// checkpoint, every other run pairs two arms.
pub(crate) fn cells_for(split: EvalSplit) -> u64 {
    match split {
        EvalSplit::Train => 1,
        EvalSplit::Validation | EvalSplit::HeldOut => 2,
    }
}

/// How far apart two validation attempts of one round start their seeds.
const ATTEMPT_SEED_STRIDE: i64 = 100;
/// How far apart two rounds start their seeds. Round 0 is the held-out run.
const ROUND_SEED_STRIDE: i64 = 1_000;

/// Seeds are spaced so a re-run and a later round never draw the same trial
/// seeds: a trial's seed is `seed_base + trial_index`. [`check_seed_spacing`]
/// refuses at freeze any job whose trials would outgrow an attempt's stride,
/// whose re-runs would outgrow a round's, or whose highest seed would not fit
/// an `i64`, so the saturation here never happens for a frozen job.
fn seed_base_for(base: i64, round: u32, attempt: u32) -> i64 {
    base.saturating_add(i64::from(round).saturating_mul(ROUND_SEED_STRIDE))
        .saturating_add(i64::from(attempt).saturating_mul(ATTEMPT_SEED_STRIDE))
}

/// Spec section 3.5: a re-run draws a new `seed_base`, and no re-run or later
/// round reuses an earlier attempt's seeds. A round's train run and its
/// validation attempt 0 do share a seed base, on disjoint splits, which the
/// spec allows. Checked once, at freeze, against what the job may
/// ever plan: rounds `0..=max_rounds`, attempts `0..=max_reruns`, and trial
/// indices `0..trials_per_case`.
pub(crate) fn check_seed_spacing(request: &JobRequest, policy: &PolicyV2) -> Result<()> {
    let trials = i64::from(request.trials_per_case);
    if trials > ATTEMPT_SEED_STRIDE {
        return Err(refused(format!(
            "trials_per_case {} exceeds {ATTEMPT_SEED_STRIDE}; a re-run would draw the trial seeds of the attempt before it",
            request.trials_per_case
        )));
    }
    let attempts = i64::from(policy.max_reruns) + 1;
    if attempts * ATTEMPT_SEED_STRIDE > ROUND_SEED_STRIDE {
        return Err(refused(format!(
            "policy max_reruns {} exceeds {}; a re-run would draw the trial seeds of the next round",
            policy.max_reruns,
            ROUND_SEED_STRIDE / ATTEMPT_SEED_STRIDE - 1
        )));
    }
    let highest = i64::from(request.budgets.max_rounds)
        .checked_mul(ROUND_SEED_STRIDE)
        .and_then(|rounds| rounds.checked_add(i64::from(policy.max_reruns) * ATTEMPT_SEED_STRIDE))
        .and_then(|offset| offset.checked_add((trials - 1).max(0)))
        .and_then(|offset| request.seed_base.checked_add(offset));
    if highest.is_none() {
        return Err(refused(format!(
            "seed_base {} leaves no room for {} rounds of seeds within an i64",
            request.seed_base, request.budgets.max_rounds
        )));
    }
    Ok(())
}

pub(crate) fn train_plan(
    job_id: &str,
    origin: &JobOrigin,
    round: u32,
    checkpoint: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{job_id}-r{round}-train"),
        split: EvalSplit::Train,
        round: Some(round),
        cells: vec![(BASELINE_CELL, checkpoint.to_path_buf())],
        seed_base: seed_base_for(origin.seed_base, round, 0),
    }
}

pub(crate) fn validation_plan(
    job_id: &str,
    origin: &JobOrigin,
    round: u32,
    attempt: u32,
    checkpoint: &Path,
    candidate: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{job_id}-r{round}-v{attempt}"),
        split: EvalSplit::Validation,
        round: Some(round),
        cells: vec![
            (BASELINE_CELL, checkpoint.to_path_buf()),
            (CANDIDATE_CELL, candidate.to_path_buf()),
        ],
        seed_base: seed_base_for(origin.seed_base, round, attempt),
    }
}

/// The one held-out run of a job: the original baseline against the final
/// checkpoint. Run once, never re-run.
pub(crate) fn held_out_plan(
    job_id: &str,
    origin: &JobOrigin,
    baseline: &Path,
    checkpoint: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{job_id}-held-out"),
        split: EvalSplit::HeldOut,
        round: None,
        cells: vec![
            (BASELINE_CELL, baseline.to_path_buf()),
            (CANDIDATE_CELL, checkpoint.to_path_buf()),
        ],
        seed_base: seed_base_for(origin.seed_base, 0, 0),
    }
}

/// What a run means comes from `origin`; how it is executed comes from
/// `request`.
pub(crate) fn run_request(request: &JobRequest, origin: &JobOrigin, plan: &RunPlan) -> RunRequest {
    RunRequest {
        run_id: plan.run_id.clone(),
        owner: origin.owner.clone(),
        evaluator_did: request.evaluator_did.clone(),
        definition_id: origin.definition.definition_id.clone(),
        split: plan.split,
        case_ids: None,
        cells: plan
            .cells
            .iter()
            .map(|(cell_id, pack)| CellRequest {
                cell_id: (*cell_id).to_owned(),
                label: (*cell_id).to_owned(),
                source: CellSource::Directory(pack.clone()),
                behavior_id: origin.subject.behavior_id.clone(),
                inference_profile_id: origin.inference_profile_id.clone(),
            })
            .collect(),
        trials_per_case: origin.trials_per_case,
        seed_base: plan.seed_base,
        deadline_secs: request.deadline_secs,
        concurrency: request.concurrency,
        max_infra_retries: request.max_infra_retries,
        breaker_threshold: request.breaker_threshold,
        purpose: format!("optimization:{}", request.job_id),
        source_commit: request.source_commit.clone(),
        source_dirty: request.source_dirty,
        // Stage evidence is what the executor captures out of a trial home, and
        // grading reads nothing else. Frozen with the job, so every run of it
        // captures the same list.
        captures: origin.captures.clone(),
        runs_dir: request.runs_dir.clone(),
    }
}

/// Freeze and run `plan`, or resume it when its row already exists and was
/// frozen from this plan ([`check_run_matches_plan`]).
pub(crate) async fn execute_run(
    access: &ConfigAccess,
    request: &JobRequest,
    origin: &JobOrigin,
    plan: &RunPlan,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: CancellationToken,
) -> Result<RunOutcome> {
    if let Some(existing) = load_run(access, &origin.owner, &plan.run_id).await? {
        let digests = plan
            .cells
            .iter()
            .map(|(_, dir)| {
                load_pack(&CellSource::Directory(dir.clone()), &origin.owner)
                    .map(|pack| pack.digest)
                    .with_context(|| format!("loading pack {}", dir.display()))
            })
            .collect::<Result<Vec<_>>>()?;
        let captures = freeze::frozen_captures(&request.runs_dir.join(&plan.run_id))?;
        check_run_matches_plan(
            &existing.origin,
            captures.as_deref(),
            &request.job_id,
            origin,
            plan,
            &digests,
        )?;
        tracing::info!(run_id = %plan.run_id, "optimization run resumed");
        return resume(
            access,
            &origin.owner,
            &plan.run_id,
            &request.runs_dir,
            executor,
            registry,
            cancel,
            &request.run_options,
        )
        .await;
    }
    run(
        access,
        &run_request(request, origin, plan),
        executor,
        registry,
        cancel,
        &request.run_options,
    )
    .await
}

pub fn split_case_count(definition: &EvalDefinition, split: EvalSplit) -> u64 {
    definition
        .cases
        .iter()
        .filter(|case| case.split == split)
        .count() as u64
}

/// How many case-trials a run of `split` costs.
pub fn run_cost(
    definition: &EvalDefinition,
    split: EvalSplit,
    trials_per_case: u32,
    cells: u64,
) -> u64 {
    split_case_count(definition, split)
        .saturating_mul(u64::from(trials_per_case))
        .saturating_mul(cells)
}

/// Each run the journal has started, once, in the order it started.
fn started_runs(journal: &[JournalEntry]) -> Vec<(&str, EvalSplit)> {
    let mut seen = BTreeSet::new();
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::RunStarted { run_id, split, .. } => Some((run_id.as_str(), *split)),
            _ => None,
        })
        .filter(|(run_id, _)| seen.insert(*run_id))
        .collect()
}

/// The runs started before the decision point that `pending` belongs to
/// (ruling T33-1), each once.
///
/// A budget check is a decision point: the driver checks, then journals the
/// first of `pending` as started. So the point sits just before the first
/// `RunStarted` of any pending run, or at the journal's end when none of them
/// has started. Everything journaled from there on happened after the check in
/// the uninterrupted order — including runs outside `pending`, such as a
/// validation re-run a crashed round had reached — and is never charged to it.
/// A resumed job therefore reads the same prior runs its uninterrupted twin
/// read at the same check.
pub(crate) fn prior_runs<'a>(
    journal: &'a [JournalEntry],
    pending: &[RunPlan],
) -> Vec<(&'a str, EvalSplit)> {
    let decision_point = journal
        .iter()
        .position(|entry| {
            matches!(entry, JournalEntry::RunStarted { run_id, .. }
                if pending.iter().any(|plan| plan.run_id == *run_id))
        })
        .unwrap_or(journal.len());
    started_runs(&journal[..decision_point])
}

/// The case-trials a budget check charges before `pending` runs: each prior
/// run (see [`prior_runs`]) at its planned cost, plus every run of `pending`
/// (P-N2, T33-1).
///
/// The charge is the same however far a crash got past the check, so a
/// resumed job passes or fails it as its uninterrupted twin did, even under a
/// budget with no slack. Planned cost rather than trial rows: the rows a run
/// has written depend on when the process stopped and on infrastructure
/// retries; the plan does not. Compare with `Budgets::max_case_trials`.
///
/// Task 4: at round start pass `[train, validation attempt 0, held-out]`;
/// before validation attempt `k` pass `[attempt k, held-out]`.
pub(crate) fn case_trials_charged(
    definition: &EvalDefinition,
    trials_per_case: u32,
    journal: &[JournalEntry],
    pending: &[RunPlan],
) -> u64 {
    let committed = prior_runs(journal, pending)
        .iter()
        .fold(0u64, |total, (_, split)| {
            total.saturating_add(run_cost(
                definition,
                *split,
                trials_per_case,
                cells_for(*split),
            ))
        });
    pending.iter().fold(committed, |total, plan| {
        total.saturating_add(run_cost(
            definition,
            plan.split,
            trials_per_case,
            plan.cells.len() as u64,
        ))
    })
}

/// The tokens a budget check charges before `pending` runs: what the prior
/// runs' trials reported (see [`prior_runs`]; T33-1). At round start that is
/// every earlier round; before validation attempt `k` it also counts this
/// round's train run and attempts `0..k`. Compare with `Budgets::max_tokens`;
/// pass the same `pending` as to [`case_trials_charged`].
pub(crate) async fn tokens_charged(
    access: &ConfigAccess,
    owner: &str,
    journal: &[JournalEntry],
    pending: &[RunPlan],
) -> Result<u64> {
    let mut tokens = 0u64;
    for (run_id, _) in prior_runs(journal, pending) {
        tokens = tokens.saturating_add(trial_tokens(&load_trials(access, owner, run_id).await?));
    }
    Ok(tokens)
}

/// The tokens `trials` reported. A trial with no completion reported none.
fn trial_tokens(trials: &[TrialRecord]) -> u64 {
    trials
        .iter()
        .filter_map(|record| record.completion.as_ref())
        .fold(0u64, |total, completion| {
            total
                .saturating_add(completion.usage.input_tokens.unwrap_or(0))
                .saturating_add(completion.usage.output_tokens.unwrap_or(0))
        })
}

/// What the job has already spent, for reporting: every trial row of every
/// run its journal started, and the tokens those trials reported. Not what a
/// budget check reads — see [`case_trials_charged`] and [`tokens_charged`].
pub async fn spend_so_far(
    access: &ConfigAccess,
    owner: &str,
    journal: &[JournalEntry],
) -> Result<Spend> {
    let mut spend = Spend::default();
    for (run_id, _) in started_runs(journal) {
        let trials = load_trials(access, owner, run_id).await?;
        spend.case_trials = spend.case_trials.saturating_add(trials.len() as u64);
        spend.tokens = spend.tokens.saturating_add(trial_tokens(&trials));
    }
    Ok(spend)
}

/// The installed definition, read and validated by the runner's own read, so
/// the driver and the runner agree on what a case is.
pub(crate) async fn load_definition(
    access: &ConfigAccess,
    owner: &str,
    definition_id: &str,
) -> Result<EvalDefinition> {
    freeze::load_definition(access, owner, definition_id).await
}

/// The same identity `eval::runner::freeze` records on a run's origin.
pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef> {
    freeze::definition_ref(definition)
}

#[cfg(test)]
tokio::task_local! {
    /// Seconds a test has advanced the driver's clock by (ruling FW-4). Task
    /// local rather than thread local, so it follows the job's future under
    /// any runtime and never reaches a test that did not scope it.
    pub(crate) static TEST_CLOCK_OFFSET: std::cell::Cell<u64>;
}

pub(crate) fn now_unix_secs() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    #[cfg(test)]
    let now = now.saturating_add(
        TEST_CLOCK_OFFSET
            .try_with(|offset| offset.get())
            .unwrap_or(0),
    );
    now
}

/// A job that will not start or resume, and why. Distinct from a `Failed`
/// job: nothing was written.
#[derive(Debug)]
pub struct JobRefused(pub String);

impl std::fmt::Display for JobRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JobRefused {}

pub fn job_refused(error: &anyhow::Error) -> Option<&JobRefused> {
    error.downcast_ref::<JobRefused>()
}

fn refused(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(JobRefused(reason.into()))
}

/// A refusal raised by the eval layer's own validation (a bad job id, a
/// missing or invalid definition) is the driver's refusal too: nothing was
/// written.
fn as_job_refusal(error: anyhow::Error) -> anyhow::Error {
    match freeze_refused(&error) {
        Some(refusal) => refused(refusal.0.clone()),
        None => error,
    }
}

/// What one pass over a job produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobOutcome {
    pub job_id: String,
    /// `Running` when the pass stopped on cancellation; call again to resume.
    pub state: JobState,
    /// The retained checkpoint, never the best candidate the job ever saw.
    pub checkpoint: Option<Checkpoint>,
    pub rounds_used: u32,
}

/// `alpha_effective = alpha / max_rounds` is a Bonferroni correction for an
/// optimizer that tries several candidates on one validation split, so the
/// divisor has to be the number of candidates the budget allows.
pub(crate) fn check_policy(request: &JobRequest, policy: &PolicyV2) -> Result<()> {
    if policy.max_rounds == request.budgets.max_rounds {
        return Ok(());
    }
    Err(refused(format!(
        "policy max_rounds {} does not match the budget's max_rounds {}; the Bonferroni divisor must be the number of candidates the job may try",
        policy.max_rounds, request.budgets.max_rounds
    )))
}

/// Finding F2: a resume must repeat the request the job was frozen from. The
/// origin is authoritative; this only refuses a caller who believes otherwise.
pub(crate) fn check_resume(
    request: &JobRequest,
    policy: &PolicyV2,
    origin: &JobOrigin,
) -> Result<()> {
    let mut differs = Vec::new();
    if &origin.policy != policy {
        differs.push("policy");
    }
    if origin.budgets != request.budgets {
        differs.push("budgets");
    }
    if origin.trials_per_case != request.trials_per_case {
        differs.push("trials_per_case");
    }
    if origin.seed_base != request.seed_base {
        differs.push("seed_base");
    }
    if origin.subject.behavior_id != request.behavior_id {
        differs.push("behavior_id");
    }
    if origin.definition.definition_id != request.definition_id {
        differs.push("definition_id");
    }
    if origin.inference_profile_id != request.inference_profile_id {
        differs.push("inference_profile_id");
    }
    if origin.max_text_bytes != request.max_text_bytes {
        differs.push("max_text_bytes");
    }
    if origin.jobs_dir != request.jobs_dir {
        differs.push("jobs_dir");
    }
    if origin.owner != request.owner {
        differs.push("owner");
    }
    if origin.captures != request.captures {
        differs.push("captures");
    }
    if differs.is_empty() {
        return Ok(());
    }
    Err(refused(format!(
        "job {} was frozen with a different {}; a resume must repeat the request the job was created from",
        request.job_id,
        differs.join(", ")
    )))
}

/// Task 3 review minor 4 (constraint 15): a run that already exists is resumed
/// only when it was frozen from the plan being executed now. `digests` holds
/// what each of the plan's cell directories digests to, in cell order, and
/// `captures` the list the run froze beside itself (`None` when it has none).
pub(crate) fn check_run_matches_plan(
    run: &RunOrigin,
    captures: Option<&[Capture]>,
    job_id: &str,
    origin: &JobOrigin,
    plan: &RunPlan,
    digests: &[String],
) -> Result<()> {
    let mut differs = Vec::new();
    if run.purpose != format!("optimization:{job_id}") {
        differs.push("purpose");
    }
    if run.definition != origin.definition {
        differs.push("definition");
    }
    if run.split != plan.split {
        differs.push("split");
    }
    if run.trials_per_case != origin.trials_per_case {
        differs.push("trials_per_case");
    }
    if run.seed_base != plan.seed_base {
        differs.push("seed_base");
    }
    let cells_match =
        run.cells.len() == plan.cells.len()
            && digests.len() == plan.cells.len()
            && run.cells.iter().zip(&plan.cells).zip(digests).all(
                |((cell, (cell_id, _)), digest)| {
                    cell.cell_id == *cell_id
                        && cell.subject.behavior_id == origin.subject.behavior_id
                        && cell.subject.pack_digest == *digest
                        && cell.inference_profile_id == origin.inference_profile_id
                },
            );
    if !cells_match {
        differs.push("cells");
    }
    if captures != Some(origin.captures.as_slice()) {
        differs.push("captures");
    }
    if differs.is_empty() {
        return Ok(());
    }
    Err(refused(format!(
        "eval run {} already exists but was frozen with a different {} than this job plans; it is not resumed as this job's run",
        plan.run_id,
        differs.join(", ")
    )))
}

pub(crate) fn proposed_for(
    journal: &[JournalEntry],
    round: u32,
) -> Option<(String, String, String)> {
    journal.iter().find_map(|entry| match entry {
        JournalEntry::Proposed {
            round: proposed,
            text,
            rationale,
            candidate_digest,
        } if *proposed == round => {
            Some((text.clone(), rationale.clone(), candidate_digest.clone()))
        }
        _ => None,
    })
}

/// A round is closed once it has been decided, structurally rejected, or
/// abandoned on budget. Replay opens and closes rounds; nothing else does.
pub(crate) fn round_is_closed(journal: &[JournalEntry], round: u32) -> bool {
    journal.iter().any(|entry| match entry {
        JournalEntry::Decided {
            round: Some(decided),
            ..
        } => *decided == round,
        JournalEntry::StructuralReject {
            round: rejected, ..
        } => *rejected == round,
        JournalEntry::BudgetExhausted {
            round: Some(exhausted),
            ..
        } => *exhausted == round,
        _ => false,
    })
}

/// Finding F7: whether the job ran out of budget is a fact of the journal.
pub(crate) fn budget_exhausted(journal: &[JournalEntry]) -> bool {
    journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { .. }))
}

/// Every digest this job has already evaluated, before `round`.
pub(crate) fn seen_digests(
    baseline_digest: &str,
    journal: &[JournalEntry],
    round: u32,
) -> Vec<String> {
    let mut digests = vec![baseline_digest.to_owned()];
    for entry in journal {
        if let JournalEntry::Proposed {
            round: proposed,
            candidate_digest,
            ..
        } = entry
        {
            if *proposed < round {
                digests.push(candidate_digest.clone());
            }
        }
    }
    digests
}

/// The `candidate_digest` of a proposal [`text_gate`] refused (ruling P-N5):
/// no pack was ever written for it, so it names the text alone. The prefix
/// keeps it out of the pack-digest namespace, so it can never make a real
/// candidate a duplicate.
pub(crate) fn text_only_digest(text: &str) -> String {
    format!("text-sha256:{:x}", Sha256::digest(text.as_bytes()))
}

/// Ruling P-N4: only an `Insufficient` verdict can change with more pairs, so
/// only it earns a re-run, and only while the policy allows one.
pub(crate) fn wants_rerun(decision: &Decision, attempt: u32, max_reruns: u32) -> bool {
    matches!(
        decision,
        Decision::Inconclusive(InconclusiveReason::Insufficient)
    ) && attempt < max_reruns
}

/// A rejection reason as the journal and the policy spell it (ruling C2).
fn reason_label(reason: &RejectReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{reason:?}"))
}

/// Every rejection the proposer is told about. `Inconclusive` and
/// budget-exhausted rounds are never negatives.
fn rejections(journal: &[JournalEntry]) -> Vec<Rejection> {
    let mut history = Vec::new();
    for entry in journal {
        let (round, reason) = match entry {
            JournalEntry::StructuralReject { round, diagnostics } => (*round, diagnostics.clone()),
            JournalEntry::Decided {
                round: Some(round),
                decision: Decision::Reject(reason),
                ..
            } => (*round, reason_label(reason)),
            _ => continue,
        };
        if let Some((text, rationale, _)) = proposed_for(journal, round) {
            history.push(Rejection {
                round,
                text,
                rationale,
                reason,
            });
        }
    }
    history
}

fn summary_of(report: &DecisionReport) -> DecisionSummary {
    DecisionSummary {
        improved: report.improved,
        tied: report.tied,
        worsened: report.worsened,
        mean_diff_bp: report.mean_diff_bp,
        p_ppm: report.p_ppm,
        alpha_effective_ppm: report.alpha_effective_ppm,
        cost_skipped: report.cost_skipped,
    }
}

fn decided(
    round: Option<u32>,
    attempt: u32,
    run_ids: &[String],
    mode: Mode,
    report: &DecisionReport,
) -> JournalEntry {
    JournalEntry::Decided {
        round,
        attempt,
        run_ids: run_ids.to_vec(),
        mode,
        decision: report.decision,
        policy_version: report.policy_version.clone(),
        summary: summary_of(report),
    }
}

fn run_started(journal: &[JournalEntry], run_id: &str) -> bool {
    journal.iter().any(|entry| {
        matches!(entry, JournalEntry::RunStarted { run_id: started, .. } if started == run_id)
    })
}

fn remove_if_present(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// Finding F7: the journaled round's candidate, in place and still digesting to
/// what `Proposed` recorded. A crash between the journal write and the rename
/// leaves only the staging directory, which is renamed here. The candidate
/// lives at [`candidate_dir`] and nowhere else (constraint 10).
fn proposed_candidate(
    origin: &JobOrigin,
    job_id: &str,
    round: u32,
    digest: &str,
) -> Result<MaterializedPack> {
    let final_dir = candidate_dir(&origin.jobs_dir, job_id, round);
    let staging = candidate_staging_dir(&origin.jobs_dir, job_id, round);
    if !final_dir.exists() {
        std::fs::rename(&staging, &final_dir).with_context(|| {
            format!(
                "round {round} was proposed but neither {} nor {} holds its candidate",
                final_dir.display(),
                staging.display()
            )
        })?;
    }
    let candidate = materialize_pack(&final_dir, &origin.owner, &origin.subject.behavior_id)?;
    anyhow::ensure!(
        candidate.digest == digest,
        "round {round}'s candidate digests to {}, not the journaled {digest}",
        candidate.digest
    );
    Ok(candidate)
}

async fn read_closure(access: &ConfigAccess, owner: &str) -> Result<Closure> {
    let owner = owner.to_owned();
    access
        .transact("optimization.capture_closure", |txn| {
            let owner = &owner;
            Box::pin(async move { capture_closure(txn, owner).await })
        })
        .await
}

/// Append `entries` in one transaction, so a crash leaves all of them or none.
/// Ruling P-N3's `Decided` and the `BudgetExhausted` that ends the rounds are
/// one fact: a resume that saw only the first would open another round.
async fn append_all(
    access: &ConfigAccess,
    job: &mut JobRecord,
    entries: Vec<JournalEntry>,
) -> Result<()> {
    {
        let (current, entries) = (&*job, &entries);
        access
            .transact("optimization.append_journal", |txn| {
                Box::pin(async move {
                    let mut advanced = current.clone();
                    for entry in entries {
                        append_in_txn(txn, &advanced, entry).await?;
                        advanced.journal.push(entry.clone());
                    }
                    Ok(())
                })
            })
            .await?;
    }
    job.journal.extend(entries);
    Ok(())
}

/// Create the job: copy the baseline pack in, cross-check its prompt against
/// the live configuration (ruling R5), freeze the origin, journal `Frozen`.
async fn freeze_job(
    access: &ConfigAccess,
    request: &JobRequest,
    policy: &PolicyV2,
) -> Result<JobRecord> {
    validate_job_id(&request.job_id)?;
    check_policy(request, policy)?;
    check_seed_spacing(request, policy)?;
    let owner = request.owner.as_str();
    let definition = load_definition(access, owner, &request.definition_id)
        .await
        .map_err(as_job_refusal)?;

    let source = materialize_pack(&request.baseline_pack, owner, &request.behavior_id)?;
    let pack_text = baseline_text(&source)?;
    // Ruling R5, before anything is written: the pack must be the live one.
    let closure = read_closure(access, owner).await?;
    let target = Target {
        field: TargetField::AgentContextSystemPrompt,
        owner: request.owner.clone(),
        id: source.context_id.clone(),
    };
    if current_text(&closure, &target)? != pack_text {
        return Err(refused(format!(
            "the live AgentContext {:?} and the baseline pack disagree about the subject's system prompt; supply a pack exported from this configuration",
            target.id
        )));
    }
    // A promotable job evaluates the live revision: everything else the trial
    // installs from the pack must be the live configuration too.
    if let Err(error) = baseline_equivalence(
        &crate::config_client::DesiredStateApplyPlan::from_pack_config(&source.config)?,
        &closure,
    ) {
        return Err(match error.downcast::<BaselineMismatch>() {
            Ok(mismatch) => refused(mismatch.to_string()),
            Err(error) => error,
        });
    }

    let baseline_path = baseline_dir(&request.jobs_dir, &request.job_id);
    // A directory with no job row is the leftover of a freeze that never
    // finished; the job does not exist, so nothing refers to it.
    remove_if_present(&baseline_path)?;
    let baseline = materialize_candidate(&source, owner, &pack_text, &baseline_path)?;
    anyhow::ensure!(
        baseline_text(&baseline)? == pack_text,
        "the job's baseline copy at {} does not read back the pack's prompt",
        baseline_path.display()
    );

    let origin = JobOrigin {
        target,
        closure: closure_digests(&closure)?,
        subject: SubjectRef {
            pack_digest: baseline.digest.clone(),
            behavior_id: request.behavior_id.clone(),
        },
        definition: definition_ref(&definition)?,
        policy: policy.clone(),
        trials_per_case: request.trials_per_case,
        budgets: request.budgets.clone(),
        baseline_text: pack_text,
        owner: request.owner.clone(),
        seed_base: request.seed_base,
        inference_profile_id: request.inference_profile_id.clone(),
        max_text_bytes: request.max_text_bytes,
        jobs_dir: request.jobs_dir.clone(),
        captures: request.captures.clone(),
    };
    let mut job = create_job(access, &request.job_id, owner, &origin).await?;
    append(access, &mut job, JournalEntry::Frozen).await?;
    Ok(job)
}

/// Why `pending` cannot run, if it cannot: a budget check at one decision
/// point (rulings P-N2 and T33-1). Both quantities charge only runs started
/// before this point in the uninterrupted order, so a resumed job reads the
/// same reason its twin did.
async fn unaffordable(
    access: &ConfigAccess,
    definition: &EvalDefinition,
    origin: &JobOrigin,
    journal: &[JournalEntry],
    pending: &[RunPlan],
) -> Result<Option<String>> {
    let budgets = &origin.budgets;
    let case_trials = case_trials_charged(definition, origin.trials_per_case, journal, pending);
    if case_trials > budgets.max_case_trials {
        return Ok(Some(format!(
            "{case_trials} case-trials charged, including the reserved held-out run, exceed the budget of {}",
            budgets.max_case_trials
        )));
    }
    let tokens = tokens_charged(access, &origin.owner, journal, pending).await?;
    if tokens > budgets.max_tokens {
        return Ok(Some(format!(
            "{tokens} tokens already spent exceed the budget of {}",
            budgets.max_tokens
        )));
    }
    // The deadline gates rounds, never confirmation: the held-out run is
    // exempt from it as from the token budget (rulings T33-2 and F-3). See the
    // finalize step of `run_job`.
    if past_deadline(budgets) {
        return Ok(Some("the job's deadline has passed".to_owned()));
    }
    Ok(None)
}

/// Decide over `run_ids`, read from their rows, and say whether any of them
/// is invalidated. A run that has been invalidated is left out of the
/// evidence; the seed still follows every run the decision names. The driver
/// decides and `optimization::show` recomputes through this one path.
pub(crate) async fn decide_runs(
    access: &ConfigAccess,
    owner: &str,
    definition: &EvalDefinition,
    policy: &PolicyV2,
    mode: Mode,
    run_ids: &[String],
) -> Result<(DecisionReport, bool)> {
    let mut runs = Vec::with_capacity(run_ids.len());
    let mut invalidated = false;
    for run_id in run_ids {
        let rows = load_run_rows(access, owner, run_id).await?;
        if rows.invalidated {
            invalidated = true;
            tracing::warn!(
                run_id,
                "optimization decision leaves out an invalidated run"
            );
            continue;
        }
        runs.push(rows);
    }
    let evidence = decision_evidence(definition, &runs, policy.max_missing_usage_bp);
    let report = decide(mode, policy, &evidence, decision_seed(run_ids));
    if report.cost_skipped {
        tracing::info!(
            ?run_ids,
            "optimization cost gate skipped; the decision records cost_skipped"
        );
    }
    Ok((report, invalidated))
}

/// Start or resume a job, run rounds, and finalize.
///
/// Calling this again on the same `job_id` resumes: the journal is replayed,
/// an unfinished run is resumed through the runner, and a proposed round is
/// never proposed again. A resumed job and an uninterrupted one reach the same
/// journal.
pub async fn run_job(
    access: &ConfigAccess,
    request: &JobRequest,
    executor: &dyn TrialExecutor,
    proposer: &dyn Proposer,
    registry: &CheckRegistry,
    policy: &PolicyV2,
    cancel: CancellationToken,
) -> Result<JobOutcome> {
    // Ruling P-N6: no path is built from a job id this has not accepted.
    validate_job_id(&request.job_id)?;
    let mut job = match load_job(access, &request.owner, &request.job_id).await? {
        Some(mut existing) => {
            check_resume(request, policy, &existing.origin)?;
            // Ruling T34-3: `create_job` and `Frozen` are two transactions. A
            // crash between them leaves the row with an empty journal; the
            // origin is already frozen, so `Frozen` is all that is missing.
            if existing.journal.is_empty() {
                tracing::info!(job_id = %existing.job_id, "optimization job resumed before Frozen was journaled");
                append(access, &mut existing, JournalEntry::Frozen).await?;
            }
            existing
        }
        None => freeze_job(access, request, policy).await?,
    };
    // From here on, only the origin decides (finding F2).
    let origin = job.origin.clone();
    let owner = origin.owner.as_str();
    let job_id = job.job_id.clone();

    let state = derive_state(&job.journal);
    if state != JobState::Running {
        return Ok(outcome(&job, state));
    }

    // Finding F5: the instrument first, then the subject. A definition that
    // was deleted or no longer validates has changed too (ruling T34-4); any
    // other error is not a fact about the definition and propagates.
    let definition = match load_definition(access, owner, &origin.definition.definition_id).await {
        Ok(definition) => definition,
        Err(error) if freeze_refused(&error).is_some() => {
            tracing::warn!(job_id = %job_id, error = %format!("{error:#}"), "optimization job's definition is gone or invalid");
            let state = JobState::Failed {
                reason: "definition_changed".into(),
            };
            return finalize(access, &mut job, state).await;
        }
        Err(error) => return Err(error),
    };
    if definition_ref(&definition)? != origin.definition {
        let state = JobState::Failed {
            reason: "definition_changed".into(),
        };
        return finalize(access, &mut job, state).await;
    }
    if closure_digests(&read_closure(access, owner).await?)? != origin.closure {
        let state = JobState::Failed {
            reason: "baseline_drifted".into(),
        };
        return finalize(access, &mut job, state).await;
    }
    let baseline_path = baseline_dir(&origin.jobs_dir, &job_id);
    let baseline = materialize_pack(&baseline_path, owner, &origin.subject.behavior_id)?;
    if baseline.digest != origin.subject.pack_digest {
        return Err(refused(format!(
            "the job's baseline copy at {} no longer digests to what it froze",
            baseline_path.display()
        )));
    }

    let policy = &origin.policy;
    let budgets = &origin.budgets;
    // Cancellation stops after the run in progress; the job stays `Running`
    // and the next call resumes it (ruling R9).
    let stopped = |job: &JobRecord| -> Result<JobOutcome> { Ok(outcome(job, JobState::Running)) };

    'rounds: for round in 1..=budgets.max_rounds {
        if budget_exhausted(&job.journal) {
            break;
        }
        if round_is_closed(&job.journal, round) {
            continue;
        }
        let retained = checkpoint(&job.journal);
        let (checkpoint_path, checkpoint_text) = match &retained {
            Some(held) => (
                verified_checkpoint(&origin, &job_id, held)?,
                held.text.clone(),
            ),
            None => (baseline_path.clone(), origin.baseline_text.clone()),
        };
        let candidate_path = candidate_dir(&origin.jobs_dir, &job_id, round);
        let held_out = held_out_plan(&job_id, &origin, &baseline_path, &checkpoint_path);

        // 1. Train run: one cell, the checkpoint, on the train split. A round
        // starts only if it can finish beside the reserved held-out run; once
        // its train run is journaled, that check has been passed.
        let train = train_plan(&job_id, &origin, round, &checkpoint_path);
        if !run_started(&job.journal, &train.run_id) {
            let first = validation_plan(
                &job_id,
                &origin,
                round,
                0,
                &checkpoint_path,
                &candidate_path,
            );
            let pending = [train.clone(), first, held_out.clone()];
            if let Some(reason) =
                unaffordable(access, &definition, &origin, &job.journal, &pending).await?
            {
                tracing::info!(round, %reason, "optimization round not affordable");
                let entry = JournalEntry::BudgetExhausted {
                    round: Some(round),
                    reason,
                };
                append(access, &mut job, entry).await?;
                break;
            }
            if cancel.is_cancelled() {
                return stopped(&job);
            }
            let entry = JournalEntry::RunStarted {
                run_id: train.run_id.clone(),
                round: Some(round),
                split: EvalSplit::Train,
            };
            append(access, &mut job, entry).await?;
        }
        let ran = execute_run(
            access,
            request,
            &origin,
            &train,
            executor,
            registry,
            cancel.clone(),
        )
        .await?;
        // Ruling F1: a run its cancel marker stopped is unfinished. The job
        // stops at it, journals nothing more, and a resume continues it.
        if cancel.is_cancelled() || ran.cancelled {
            return stopped(&job);
        }

        // 2. Propose. Feedback comes from this round's train run and nowhere
        // else (constraint 12).
        let (text, digest) = match proposed_for(&job.journal, round) {
            Some((text, _, digest)) => (text, digest),
            None => {
                // Finding F7: a directory with no `Proposed` is a leftover.
                let staging = candidate_staging_dir(&origin.jobs_dir, &job_id, round);
                remove_if_present(&staging)?;
                remove_if_present(&candidate_path)?;
                let train_rows = load_run_rows(access, owner, &train.run_id).await?;
                let proposal = proposer
                    .propose(ProposalInput {
                        round,
                        current_text: checkpoint_text.clone(),
                        feedback: train_feedback(&definition, &train_rows),
                        rejections: rejections(&job.journal),
                        max_text_bytes: origin.max_text_bytes,
                    })
                    .await?;
                // Ruling P-N5: a text the gate refuses is never materialized.
                let digest = match text_gate(&baseline, &proposal.text, origin.max_text_bytes) {
                    Ok(()) => {
                        materialize_candidate(&baseline, owner, &proposal.text, &staging)?.digest
                    }
                    Err(_) => text_only_digest(&proposal.text),
                };
                let entry = JournalEntry::Proposed {
                    round,
                    text: proposal.text.clone(),
                    rationale: proposal.rationale.clone(),
                    candidate_digest: digest.clone(),
                };
                append(access, &mut job, entry).await?;
                (proposal.text, digest)
            }
        };

        // 3. Structural gate, before any validation spend: the text alone,
        // then the materialized candidate against the baseline.
        let gated = match text_gate(&baseline, &text, origin.max_text_bytes) {
            Err(rejection) => Err(rejection),
            Ok(()) => {
                let candidate = proposed_candidate(&origin, &job_id, round, &digest)?;
                structural_gate(
                    &baseline,
                    &candidate,
                    &text,
                    origin.max_text_bytes,
                    &seen_digests(&baseline.digest, &job.journal, round),
                )
                .map(|()| candidate)
            }
        };
        let candidate = match gated {
            Ok(candidate) => candidate,
            Err(rejection) => {
                tracing::info!(
                    round,
                    reason = rejection.reason,
                    "candidate rejected before any spend"
                );
                let entry = JournalEntry::StructuralReject {
                    round,
                    diagnostics: rejection.diagnostics(),
                };
                append(access, &mut job, entry).await?;
                continue;
            }
        };

        // 4. Validation runs, and 5. the decision. A re-run adds pairs and
        // never replaces them (finding F1).
        let mut run_ids = Vec::new();
        let mut attempt = 0;
        loop {
            let plan = validation_plan(
                &job_id,
                &origin,
                round,
                attempt,
                &checkpoint_path,
                &candidate.dir,
            );
            // Attempt 0 was charged at round start, and every later attempt
            // just before it was journaled below.
            if !run_started(&job.journal, &plan.run_id) {
                if cancel.is_cancelled() {
                    return stopped(&job);
                }
                let entry = JournalEntry::RunStarted {
                    run_id: plan.run_id.clone(),
                    round: Some(round),
                    split: EvalSplit::Validation,
                };
                append(access, &mut job, entry).await?;
            }
            let ran = execute_run(
                access,
                request,
                &origin,
                &plan,
                executor,
                registry,
                cancel.clone(),
            )
            .await?;
            // Ruling F1: a run its cancel marker stopped is unfinished. The job
            // stops at it, journals nothing more, and a resume continues it.
            if cancel.is_cancelled() || ran.cancelled {
                return stopped(&job);
            }
            run_ids.push(plan.run_id.clone());
            let (report, _) =
                decide_runs(access, owner, &definition, policy, Mode::Improve, &run_ids).await?;

            if !wants_rerun(&report.decision, attempt, policy.max_reruns) {
                let entry = decided(Some(round), attempt, &run_ids, Mode::Improve, &report);
                append(access, &mut job, entry).await?;
                break;
            }
            let rerun = validation_plan(
                &job_id,
                &origin,
                round,
                attempt + 1,
                &checkpoint_path,
                &candidate.dir,
            );
            if !run_started(&job.journal, &rerun.run_id) {
                let pending = [rerun.clone(), held_out.clone()];
                if let Some(reason) =
                    unaffordable(access, &definition, &origin, &job.journal, &pending).await?
                {
                    // Ruling P-N3: the completed run keeps its real decision,
                    // and the rounds end.
                    tracing::info!(round, attempt, %reason, "optimization re-run not affordable");
                    let entries = vec![
                        decided(Some(round), attempt, &run_ids, Mode::Improve, &report),
                        JournalEntry::BudgetExhausted {
                            round: None,
                            reason,
                        },
                    ];
                    append_all(access, &mut job, entries).await?;
                    break 'rounds;
                }
                if cancel.is_cancelled() {
                    return stopped(&job);
                }
                let entry = JournalEntry::RunStarted {
                    run_id: rerun.run_id.clone(),
                    round: Some(round),
                    split: EvalSplit::Validation,
                };
                append(access, &mut job, entry).await?;
            }
            attempt += 1;
        }
    }

    // 6. Finalize. The held-out split is touched once, and only when a round
    // moved the checkpoint off the baseline.
    let Some(retained) = checkpoint(&job.journal) else {
        let state = if budget_exhausted(&job.journal) {
            JobState::Exhausted
        } else {
            JobState::NothingToPromote
        };
        return finalize(access, &mut job, state).await;
    };
    let decision = match decided_held_out(&job.journal) {
        Some(decision) => decision,
        None => {
            // Rulings T33-2 and F-3: once validation has accepted, the
            // held-out confirmation always runs. It is reserved in every
            // round's case-trial charge, and it is checked against neither the
            // token budget nor the deadline: a job whose rounds ended on
            // either still confirms its checkpoint, so an accepted candidate
            // never reaches `ReadyToPromote` unconfirmed or is left `Running`.
            //
            // Ruling T34-5: the held-out run freezes the pack the journal
            // retained, or none.
            let checkpoint_path = verified_checkpoint(&origin, &job_id, &retained)?;
            let plan = held_out_plan(&job_id, &origin, &baseline_path, &checkpoint_path);
            if !run_started(&job.journal, &plan.run_id) {
                if cancel.is_cancelled() {
                    return stopped(&job);
                }
                let entry = JournalEntry::RunStarted {
                    run_id: plan.run_id.clone(),
                    round: None,
                    split: EvalSplit::HeldOut,
                };
                append(access, &mut job, entry).await?;
            }
            let ran = execute_run(
                access,
                request,
                &origin,
                &plan,
                executor,
                registry,
                cancel.clone(),
            )
            .await?;
            // Ruling F1: a run its cancel marker stopped is unfinished. The job
            // stops at it, journals nothing more, and a resume continues it.
            if cancel.is_cancelled() || ran.cancelled {
                return stopped(&job);
            }
            let run_ids = vec![plan.run_id.clone()];
            let (report, _) =
                decide_runs(access, owner, &definition, policy, Mode::Confirm, &run_ids).await?;
            append(
                access,
                &mut job,
                decided(None, 0, &run_ids, Mode::Confirm, &report),
            )
            .await?;
            report.decision
        }
    };
    let state = match decision {
        Decision::Accept => JobState::ReadyToPromote,
        Decision::Reject(_) => JobState::Failed {
            reason: "held_out_regression".into(),
        },
        Decision::Inconclusive(_) => JobState::Failed {
            reason: "held_out_inconclusive".into(),
        },
    };
    finalize(access, &mut job, state).await
}

/// The retained checkpoint's directory, still digesting to what the journal
/// retained. A missing or edited checkpoint is an error, never rebuilt.
pub(crate) fn verified_checkpoint(
    origin: &JobOrigin,
    job_id: &str,
    held: &Checkpoint,
) -> Result<PathBuf> {
    let path = candidate_dir(&origin.jobs_dir, job_id, held.round);
    let pack = materialize_pack(&path, &origin.owner, &origin.subject.behavior_id)?;
    anyhow::ensure!(
        pack.digest == held.pack_digest,
        "the checkpoint at {} no longer digests to the journaled {}",
        path.display(),
        held.pack_digest
    );
    Ok(path)
}

fn past_deadline(budgets: &Budgets) -> bool {
    budgets
        .deadline_unix_secs
        .is_some_and(|deadline| now_unix_secs() > deadline)
}

fn decided_held_out(journal: &[JournalEntry]) -> Option<Decision> {
    journal.iter().find_map(|entry| match entry {
        JournalEntry::Decided {
            round: None,
            mode: Mode::Confirm,
            decision,
            ..
        } => Some(*decision),
        _ => None,
    })
}

async fn finalize(
    access: &ConfigAccess,
    job: &mut JobRecord,
    state: JobState,
) -> Result<JobOutcome> {
    tracing::info!(job_id = %job.job_id, state = state.label(), "optimization job finalized");
    let entry = JournalEntry::Finalized {
        state: state.clone(),
    };
    append(access, job, entry).await?;
    Ok(outcome(job, state))
}

fn outcome(job: &JobRecord, state: JobState) -> JobOutcome {
    JobOutcome {
        job_id: job.job_id.clone(),
        state,
        checkpoint: checkpoint(&job.journal),
        rounds_used: rounds_used(&job.journal),
    }
}

#[cfg(test)]
mod tests {
    use super::matrix::findings_capture as findings;
    use super::*;
    use crate::eval::{DefinitionRef, SubjectRef};
    use crate::optimization::policy::PolicyV2;
    use crate::optimization::target::{Target, TargetField};
    use serde_json::json;
    use std::path::PathBuf;

    fn definition() -> EvalDefinition {
        let case = |case_id: &str, split: &str| {
            json!({
                "case_id": case_id,
                "split": split,
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [{"check": "captured_rows_count", "params": {"name": "findings", "min": 1}, "tier": "acceptance"}],
                }],
            })
        };
        serde_json::from_value(json!({
            "definition_id": "monitor-findings",
            "agent_did": "did:key:o",
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [
                case("train-a", "train"),
                case("val-a", "validation"),
                case("val-b", "validation"),
                case("held-a", "held_out"),
            ],
        }))
        .unwrap()
    }

    fn budgets() -> Budgets {
        Budgets {
            max_rounds: 3,
            max_case_trials: 1_000,
            max_tokens: 1_000_000,
            deadline_unix_secs: None,
        }
    }

    pub(super) fn request() -> JobRequest {
        JobRequest {
            job_id: "job-1".into(),
            owner: "did:key:o".into(),
            evaluator_did: "did:key:home".into(),
            behavior_id: "monitor".into(),
            definition_id: "monitor-findings".into(),
            inference_profile_id: "local".into(),
            baseline_pack: PathBuf::from("/tmp/baseline"),
            trials_per_case: 2,
            budgets: budgets(),
            max_text_bytes: 32 * 1024,
            seed_base: 1_000,
            jobs_dir: PathBuf::from("/home/eval/jobs"),
            runs_dir: PathBuf::from("/home/eval/runs"),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            deadline_secs: Some(600),
            captures: vec![findings()],
            run_options: RunOptions::default(),
        }
    }

    /// The origin `request()` would freeze.
    pub(super) fn origin() -> JobOrigin {
        JobOrigin {
            target: Target {
                field: TargetField::AgentContextSystemPrompt,
                owner: "did:key:o".into(),
                id: "monitor-context".into(),
            },
            closure: Vec::new(),
            subject: SubjectRef {
                pack_digest: "sha256:baseline".into(),
                behavior_id: "monitor".into(),
            },
            definition: DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:definition".into(),
            },
            policy: PolicyV2::uncalibrated(),
            trials_per_case: 2,
            budgets: budgets(),
            baseline_text: "Watch the mailbox.\n".into(),
            owner: "did:key:o".into(),
            seed_base: 1_000,
            inference_profile_id: "local".into(),
            max_text_bytes: 32 * 1024,
            jobs_dir: PathBuf::from("/home/eval/jobs"),
            captures: vec![findings()],
        }
    }

    #[test]
    fn a_train_run_has_one_cell_and_a_validation_run_has_two_on_one_seed() {
        let origin = origin();
        let baseline = baseline_dir(&origin.jobs_dir, "job-1");
        assert_eq!(baseline, PathBuf::from("/home/eval/jobs/job-1/baseline"));

        let train = train_plan("job-1", &origin, 1, &baseline);
        assert_eq!(train.split, EvalSplit::Train);
        assert_eq!(train.cells.len(), 1);
        assert_eq!(train.cells[0].0, BASELINE_CELL);
        assert_eq!(train.run_id, "job-1-r1-train");

        let candidate = candidate_dir(&origin.jobs_dir, "job-1", 1);
        assert_eq!(
            candidate,
            PathBuf::from("/home/eval/jobs/job-1/rounds/1/candidate")
        );
        let validation = validation_plan("job-1", &origin, 1, 0, &baseline, &candidate);
        assert_eq!(validation.run_id, "job-1-r1-v0");
        assert_eq!(
            validation
                .cells
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![BASELINE_CELL, CANDIDATE_CELL]
        );

        let frozen = run_request(&request(), &origin, &validation);
        assert_eq!(frozen.purpose, "optimization:job-1");
        assert_eq!(frozen.cells.len(), 2);
        assert_eq!(
            frozen.cells[0].label, BASELINE_CELL,
            "labels are what scripts key on"
        );
        assert_eq!(
            frozen.cells[0].inference_profile_id, frozen.cells[1].inference_profile_id,
            "both arms run the same inference binding"
        );
        assert_eq!(
            frozen.captures,
            vec![findings()],
            "C1: the job's captures reach every run it asks for"
        );
        assert_eq!(frozen.seed_base, validation.seed_base);
    }

    /// F2: a run's meaning comes from the frozen origin, never the request.
    #[test]
    fn a_run_request_reads_what_a_run_means_from_the_origin() {
        let mut drifted = request();
        drifted.trials_per_case = 5;
        drifted.seed_base = 9_999;
        drifted.behavior_id = "other".into();
        drifted.inference_profile_id = "other".into();
        drifted.definition_id = "other".into();
        drifted.captures.clear();
        let origin = origin();
        let plan = validation_plan("job-1", &origin, 1, 0, Path::new("a"), Path::new("b"));
        let frozen = run_request(&drifted, &origin, &plan);
        assert_eq!(frozen.trials_per_case, 2);
        assert_eq!(frozen.captures, origin.captures);
        assert_eq!(frozen.definition_id, "monitor-findings");
        assert!(frozen
            .cells
            .iter()
            .all(|cell| cell.behavior_id == "monitor" && cell.inference_profile_id == "local"));
        assert_eq!(
            plan.seed_base,
            validation_plan("job-1", &origin, 1, 0, Path::new("a"), Path::new("b")).seed_base
        );
        assert!(plan.seed_base < 9_999, "the seed follows origin.seed_base");
    }

    #[test]
    fn a_rerun_draws_a_new_seed_base_and_a_later_round_never_collides() {
        let origin = origin();
        let seed = |round, attempt| {
            validation_plan(
                "job-1",
                &origin,
                round,
                attempt,
                Path::new("a"),
                Path::new("b"),
            )
            .seed_base
        };
        let (first, rerun, next_round) = (seed(1, 0), seed(1, 1), seed(2, 0));
        assert_ne!(first, rerun);
        assert_ne!(first, next_round);
        assert_ne!(rerun, next_round);
        // A trial draws `seed_base + trial_index`, so the gaps must be wider
        // than any run's trials per case.
        assert!(rerun - first >= 100, "{first} {rerun}");
        assert!(next_round - first >= 1_000, "{first} {next_round}");
    }

    #[test]
    fn a_run_costs_its_split_times_its_trials_times_its_cells() {
        let definition = definition();
        assert_eq!(split_case_count(&definition, EvalSplit::Validation), 2);
        assert_eq!(split_case_count(&definition, EvalSplit::HeldOut), 1);
        assert_eq!(run_cost(&definition, EvalSplit::Train, 2, 1), 2);
        assert_eq!(run_cost(&definition, EvalSplit::Validation, 2, 2), 8);
        assert_eq!(run_cost(&definition, EvalSplit::HeldOut, 2, 2), 4);
    }

    #[test]
    fn the_held_out_run_compares_the_original_baseline_with_the_final_checkpoint() {
        let origin = origin();
        let plan = held_out_plan(
            "job-1",
            &origin,
            &baseline_dir(&origin.jobs_dir, "job-1"),
            &candidate_dir(&origin.jobs_dir, "job-1", 2),
        );
        assert_eq!(plan.run_id, "job-1-held-out");
        assert_eq!(plan.split, EvalSplit::HeldOut);
        assert_eq!(plan.round, None);
        assert_eq!(
            plan.cells[0].1,
            PathBuf::from("/home/eval/jobs/job-1/baseline")
        );
        assert_eq!(
            plan.cells[1].1,
            PathBuf::from("/home/eval/jobs/job-1/rounds/2/candidate")
        );
    }

    /// P-N6: a job id names one directory the job owns.
    #[test]
    fn a_job_id_must_be_one_ordinary_path_component() {
        for bad in ["", " ", ".", "..", "a/b", "/abs", "../escape", "a\\b"] {
            let error = validate_job_id(bad).unwrap_err();
            assert!(
                job_refused(&error).is_some(),
                "{bad:?}: every bad id is one error type"
            );
            assert!(
                format!("{error:#}").contains("job_id"),
                "{bad:?}: {error:#}"
            );
        }
        validate_job_id("job-1").unwrap();
        assert_eq!(
            job_dir(Path::new("/home/eval/jobs"), "job-1"),
            PathBuf::from("/home/eval/jobs/job-1")
        );
        assert_eq!(
            candidate_staging_dir(Path::new("/home/eval/jobs"), "job-1", 3),
            PathBuf::from("/home/eval/jobs/job-1/rounds/3/candidate.staging")
        );
    }

    fn started(run_id: &str, round: Option<u32>, split: EvalSplit) -> JournalEntry {
        JournalEntry::RunStarted {
            run_id: run_id.into(),
            round,
            split,
        }
    }

    /// Review minor 1: a job whose seeds could collide or overflow is refused
    /// at freeze, and the boundaries themselves are accepted.
    #[test]
    fn a_job_whose_seeds_would_collide_or_overflow_is_refused() {
        let policy = PolicyV2::uncalibrated();
        check_seed_spacing(&request(), &policy).unwrap();
        let refusal = |request: &JobRequest, policy: &PolicyV2| {
            let error = check_seed_spacing(request, policy).unwrap_err();
            job_refused(&error)
                .unwrap_or_else(|| panic!("{error:#}"))
                .0
                .clone()
        };

        let mut trials = request();
        trials.trials_per_case = 100;
        check_seed_spacing(&trials, &policy).unwrap();
        trials.trials_per_case = 101;
        assert!(refusal(&trials, &policy).contains("trials_per_case"));

        let mut reruns = policy.clone();
        reruns.max_reruns = 9;
        check_seed_spacing(&trials_at(100), &reruns).unwrap();
        reruns.max_reruns = 10;
        assert!(refusal(&request(), &reruns).contains("max_reruns"));

        // max_rounds 3, max_reruns 1, trials 2: the highest seed is
        // seed_base + 3000 + 100 + 1.
        let mut edge = request();
        edge.seed_base = i64::MAX - 3_101;
        check_seed_spacing(&edge, &policy).unwrap();
        let origin = JobOrigin {
            seed_base: edge.seed_base,
            ..origin()
        };
        let last = validation_plan("job-1", &origin, 3, 1, Path::new("a"), Path::new("b"));
        assert_eq!(last.seed_base + 1, i64::MAX, "the last trial seed fits");
        edge.seed_base += 1;
        assert!(refusal(&edge, &policy).contains("seed_base"));
        edge.seed_base = i64::MAX;
        assert!(refusal(&edge, &policy).contains("seed_base"));
    }

    fn trials_at(trials_per_case: u32) -> JobRequest {
        JobRequest {
            trials_per_case,
            ..request()
        }
    }

    #[test]
    fn a_plan_has_as_many_cells_as_its_split_charges() {
        let origin = origin();
        let (a, b) = (Path::new("a"), Path::new("b"));
        for plan in [
            train_plan("job-1", &origin, 1, a),
            validation_plan("job-1", &origin, 1, 0, a, b),
            held_out_plan("job-1", &origin, a, b),
        ] {
            assert_eq!(
                plan.cells.len() as u64,
                cells_for(plan.split),
                "{}",
                plan.run_id
            );
        }
    }

    /// P-N2: what a round is charged does not depend on how many of its runs
    /// started before a crash, so a resumed job decides as its uninterrupted
    /// twin did, even under a budget with no slack.
    #[test]
    fn a_resumed_round_is_charged_what_its_uninterrupted_twin_was() {
        let definition = definition();
        let origin = origin();
        let (a, b) = (Path::new("a"), Path::new("b"));
        let train = train_plan("job-1", &origin, 1, a);
        let validation = validation_plan("job-1", &origin, 1, 0, a, b);
        let held_out = held_out_plan("job-1", &origin, a, b);
        let round = [train.clone(), validation.clone(), held_out.clone()];

        // train 2 + validation 8 + the reserved held-out 4, and not a trial more.
        let tight = 14;
        let twin = vec![JournalEntry::Frozen];
        assert_eq!(case_trials_charged(&definition, 2, &twin, &round), tight);

        // Crashed after train and the validation run were journaled started:
        // those runs are already charged, so the round check charges them no
        // second time.
        let mut resumed = twin.clone();
        resumed.push(started(&train.run_id, Some(1), EvalSplit::Train));
        resumed.push(started(&validation.run_id, Some(1), EvalSplit::Validation));
        assert_eq!(case_trials_charged(&definition, 2, &resumed, &round), tight);
        assert_eq!(
            case_trials_charged(
                &definition,
                2,
                &resumed,
                &[validation.clone(), held_out.clone()]
            ),
            tight,
            "the validation check before the run agrees with the round check"
        );

        // A started run journaled twice is still one run.
        resumed.push(started(&validation.run_id, Some(1), EvalSplit::Validation));
        assert_eq!(case_trials_charged(&definition, 2, &resumed, &round), tight);

        // A second round charges what the first committed plus its own runs.
        let next = [
            train_plan("job-1", &origin, 2, a),
            validation_plan("job-1", &origin, 2, 0, a, b),
            held_out,
        ];
        assert_eq!(
            case_trials_charged(&definition, 2, &resumed, &next),
            10 + 14
        );
    }
    /// T33-1: a check never charges what was started after it in the
    /// uninterrupted order, even a run outside the check's own `pending`.
    #[test]
    fn a_check_charges_only_runs_started_before_it() {
        let definition = definition();
        let origin = origin();
        let (a, b) = (Path::new("a"), Path::new("b"));
        let train = train_plan("job-1", &origin, 1, a);
        let v0 = validation_plan("job-1", &origin, 1, 0, a, b);
        let v1 = validation_plan("job-1", &origin, 1, 1, a, b);
        let held_out = held_out_plan("job-1", &origin, a, b);
        let round = [train.clone(), v0.clone(), held_out.clone()];

        // The round crashed inside the re-run: the round-start check still
        // charges what the twin's did, not the re-run it has not reached.
        let resumed = vec![
            JournalEntry::Frozen,
            started(&train.run_id, Some(1), EvalSplit::Train),
            started(&v0.run_id, Some(1), EvalSplit::Validation),
            started(&v1.run_id, Some(1), EvalSplit::Validation),
        ];
        assert_eq!(case_trials_charged(&definition, 2, &resumed, &round), 14);
        assert!(prior_runs(&resumed, &round).is_empty());

        // The pre-re-run check counts through attempt 0, before and after the
        // re-run started: train 2 + v0 8 + v1 8 + held-out 4.
        let rerun = [v1.clone(), held_out];
        let twin = &resumed[..3];
        assert_eq!(case_trials_charged(&definition, 2, twin, &rerun), 22);
        assert_eq!(case_trials_charged(&definition, 2, &resumed, &rerun), 22);
        assert_eq!(
            prior_runs(twin, &rerun),
            vec![
                (train.run_id.as_str(), EvalSplit::Train),
                (v0.run_id.as_str(), EvalSplit::Validation)
            ]
        );
        assert_eq!(prior_runs(&resumed, &rerun), prior_runs(twin, &rerun));
    }

    /// T33-1 for tokens: a round resumed after its train and validation runs
    /// started passes or fails the round-start token check as its
    /// uninterrupted twin does, under a budget with no slack.
    #[test]
    fn a_resumed_round_is_charged_the_tokens_its_twin_was() {
        let origin = origin();
        let (a, b) = (Path::new("a"), Path::new("b"));
        let r1 = [
            train_plan("job-1", &origin, 1, a),
            validation_plan("job-1", &origin, 1, 0, a, b),
        ];
        let r2 = [
            train_plan("job-1", &origin, 2, a),
            validation_plan("job-1", &origin, 2, 0, a, b),
            held_out_plan("job-1", &origin, a, b),
        ];
        let reported = std::collections::BTreeMap::from([
            (r1[0].run_id.clone(), 100u64),
            (r1[1].run_id.clone(), 400),
            (r2[0].run_id.clone(), 50),
            (r2[1].run_id.clone(), 300),
        ]);
        let tokens = |journal: &[JournalEntry], pending: &[RunPlan]| -> u64 {
            prior_runs(journal, pending)
                .iter()
                .map(|(run_id, _)| reported[*run_id])
                .sum()
        };

        let mut twin = vec![JournalEntry::Frozen];
        for plan in &r1 {
            twin.push(started(&plan.run_id, plan.round, plan.split));
        }
        let mut resumed = twin.clone();
        for plan in &r2[..2] {
            resumed.push(started(&plan.run_id, plan.round, plan.split));
        }

        let max_tokens = 500;
        assert_eq!(tokens(&twin, &r2), 500);
        assert_eq!(tokens(&resumed, &r2), 500);
        assert!(
            tokens(&resumed, &r2) <= max_tokens,
            "the twin continued at 500, so the resumed job continues"
        );
        let all_started: u64 = reported.values().sum();
        assert!(all_started > max_tokens, "counting round 2 would diverge");
    }

    #[test]
    fn trial_tokens_saturate_and_skip_unfinished_trials() {
        use crate::eval::{Anchor, TrialCompletion, TrialIdentity, TrialUsage};
        let trial = |usage: Option<TrialUsage>| TrialRecord {
            identity: TrialIdentity {
                trial_id: "t".into(),
                run_id: "r".into(),
                cell_id: BASELINE_CELL.into(),
                case_id: "train-a".into(),
                trial_index: 0,
                attempt: 0,
                trial_agent_did: "did:key:t".into(),
                session_id: "s".into(),
                seed: 0,
                home_hint: None,
            },
            created_at: String::new(),
            completion: usage.map(|usage| TrialCompletion {
                ended_at: String::new(),
                stages: Vec::new(),
                usage,
                anchor: Anchor {
                    terminal_states: Vec::new(),
                    requests: 0,
                    inference_calls: 0,
                },
                evidence_digest: None,
            }),
        };
        let usage = |input, output| TrialUsage {
            input_tokens: input,
            output_tokens: output,
        };
        assert_eq!(
            trial_tokens(&[trial(Some(usage(Some(3), None))), trial(None)]),
            3
        );
        assert_eq!(
            trial_tokens(&[trial(Some(usage(Some(u64::MAX), Some(1))))]),
            u64::MAX
        );
    }

    #[test]
    fn a_policy_that_disagrees_with_the_round_budget_is_refused() {
        let mut policy = PolicyV2::uncalibrated();
        policy.max_rounds = 5;
        let error = check_policy(&request(), &policy).unwrap_err();
        let refusal = job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(refusal.0.contains("max_rounds"), "{}", refusal.0);

        policy.max_rounds = 3;
        check_policy(&request(), &policy).unwrap();
    }

    /// F2: a resume must repeat the request the job was frozen from.
    #[test]
    fn a_resume_that_disagrees_with_the_origin_is_refused_by_field() {
        let policy = PolicyV2::uncalibrated();
        check_resume(&request(), &policy, &origin()).unwrap();

        let mut drifted = request();
        drifted.seed_base = 7;
        drifted.trials_per_case = 3;
        let error = check_resume(&drifted, &policy, &origin()).unwrap_err();
        let refusal = job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(refusal.0.contains("seed_base"), "{}", refusal.0);
        assert!(refusal.0.contains("trials_per_case"), "{}", refusal.0);

        let other_policy = PolicyV2 {
            alpha_ppm: 10_000,
            ..PolicyV2::uncalibrated()
        };
        let error = check_resume(&request(), &other_policy, &origin()).unwrap_err();
        assert!(job_refused(&error).unwrap().0.contains("policy"));
    }

    /// C1 (constraint 15): the capture list is frozen with the job, so a
    /// resume that names another list is refused, and an empty list — the
    /// request-level fallback left unused — is a list like any other.
    #[test]
    fn a_resume_whose_captures_differ_is_refused() {
        let policy = PolicyV2::uncalibrated();
        let mut dropped = request();
        dropped.captures.clear();
        let error = check_resume(&dropped, &policy, &origin()).unwrap_err();
        let refusal = job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(refusal.0.contains("captures"), "{}", refusal.0);

        let mut renamed = request();
        renamed.captures = vec![Capture::File {
            name: "findings".into(),
            glob: "*.json".into(),
        }];
        let error = check_resume(&renamed, &policy, &origin()).unwrap_err();
        assert!(job_refused(&error).unwrap().0.contains("captures"));

        let empty_origin = JobOrigin {
            captures: Vec::new(),
            ..origin()
        };
        check_resume(&dropped, &policy, &empty_origin).unwrap();
    }

    #[test]
    fn a_round_is_replayed_from_its_journal_rather_than_reproposed() {
        let journal = vec![
            JournalEntry::Frozen,
            JournalEntry::RunStarted {
                run_id: "job-1-r1-train".into(),
                round: Some(1),
                split: EvalSplit::Train,
            },
            JournalEntry::Proposed {
                round: 1,
                text: "candidate one".into(),
                rationale: "widen".into(),
                candidate_digest: "sha256:one".into(),
            },
        ];
        let proposed = proposed_for(&journal, 1).expect("round 1 already proposed");
        assert_eq!(
            (proposed.0.as_str(), proposed.2.as_str()),
            ("candidate one", "sha256:one")
        );
        assert!(
            !round_is_closed(&journal, 1),
            "no Decided and no StructuralReject"
        );
        assert!(proposed_for(&journal, 2).is_none());
        assert!(!budget_exhausted(&journal));

        let mut closed = journal.clone();
        closed.push(JournalEntry::StructuralReject {
            round: 1,
            diagnostics: "duplicate_candidate: already evaluated".into(),
        });
        assert!(round_is_closed(&closed, 1));

        closed.push(JournalEntry::BudgetExhausted {
            round: Some(2),
            reason: "no budget".into(),
        });
        assert!(
            budget_exhausted(&closed),
            "F7: exhaustion is read from the journal"
        );
    }

    #[test]
    fn the_seen_digests_are_the_baseline_and_every_earlier_candidate() {
        let proposed = |round: u32, digest: &str| JournalEntry::Proposed {
            round,
            text: String::new(),
            rationale: String::new(),
            candidate_digest: digest.into(),
        };
        let journal = vec![
            JournalEntry::Frozen,
            proposed(1, "sha256:one"),
            proposed(2, "sha256:two"),
        ];
        assert_eq!(
            seen_digests("sha256:baseline", &journal, 3),
            vec!["sha256:baseline", "sha256:one", "sha256:two"]
        );
        assert_eq!(
            seen_digests("sha256:baseline", &journal, 2),
            vec!["sha256:baseline", "sha256:one"],
            "a replayed round never compares itself against its own digest"
        );
    }

    /// Ruling C2: the proposer reads a rejection reason as the journal spells
    /// it, and never an `Inconclusive` round.
    #[test]
    fn a_rejection_reaches_the_proposer_in_its_serde_spelling() {
        let proposed = |round: u32| JournalEntry::Proposed {
            round,
            text: format!("candidate {round}"),
            rationale: "why".into(),
            candidate_digest: format!("sha256:{round}"),
        };
        let decided = |round: u32, decision: Decision| JournalEntry::Decided {
            round: Some(round),
            attempt: 0,
            run_ids: vec![format!("job-1-r{round}-v0")],
            mode: Mode::Improve,
            decision,
            policy_version: "v2".into(),
            summary: DecisionSummary {
                improved: 0,
                tied: 0,
                worsened: 0,
                mean_diff_bp: None,
                p_ppm: None,
                alpha_effective_ppm: 0,
                cost_skipped: false,
            },
        };
        let journal = vec![
            JournalEntry::Frozen,
            proposed(1),
            decided(1, Decision::Reject(RejectReason::NoImprovement)),
            proposed(2),
            decided(2, Decision::Inconclusive(InconclusiveReason::Insufficient)),
            proposed(3),
            JournalEntry::StructuralReject {
                round: 3,
                diagnostics: "empty_text: a candidate prompt must say something".into(),
            },
        ];
        let history = rejections(&journal);
        let reasons: Vec<_> = history
            .iter()
            .map(|entry| (entry.round, entry.reason.as_str()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                (1, "no_improvement"),
                (3, "empty_text: a candidate prompt must say something")
            ]
        );
    }

    /// Ruling P-N4: more pairs can only cure `Insufficient`.
    #[test]
    fn only_an_insufficient_verdict_is_rerun_and_only_within_the_policy() {
        let insufficient = Decision::Inconclusive(InconclusiveReason::Insufficient);
        assert!(wants_rerun(&insufficient, 0, 1));
        assert!(!wants_rerun(&insufficient, 1, 1), "re-runs are capped");
        for final_verdict in [
            Decision::Inconclusive(InconclusiveReason::TooFewCases),
            Decision::Inconclusive(InconclusiveReason::UnscalableEvidence),
            Decision::Accept,
            Decision::Reject(RejectReason::NoImprovement),
        ] {
            assert!(!wants_rerun(&final_verdict, 0, 1), "{final_verdict:?}");
        }
    }

    /// Ruling P-N5: a refused text has a digest of its own that no pack can
    /// share, so it never makes a later candidate a duplicate.
    #[test]
    fn a_text_refused_before_materialization_digests_outside_the_pack_namespace() {
        let digest = text_only_digest("");
        assert!(digest.starts_with("text-sha256:"), "{digest}");
        assert_eq!(digest, text_only_digest(""));
        assert_ne!(digest, text_only_digest(" "));
        assert_eq!(digest.len(), "text-sha256:".len() + 64);
    }

    /// Constraint 10: a candidate is handed to the runner as the directory
    /// pack under the job's round, and nowhere else.
    #[test]
    fn a_candidate_reaches_the_runner_as_its_rounds_directory_pack() {
        let origin = origin();
        let baseline = baseline_dir(&origin.jobs_dir, "job-1");
        let candidate = candidate_dir(&origin.jobs_dir, "job-1", 2);
        let plan = validation_plan("job-1", &origin, 2, 0, &baseline, &candidate);
        let frozen = run_request(&request(), &origin, &plan);
        let sources: Vec<_> = frozen
            .cells
            .iter()
            .map(|cell| match &cell.source {
                CellSource::Directory(dir) => (cell.cell_id.as_str(), dir.clone()),
                CellSource::InstalledPack { name } => panic!("installed pack {name}"),
            })
            .collect();
        assert_eq!(
            sources,
            vec![
                (
                    BASELINE_CELL,
                    PathBuf::from("/home/eval/jobs/job-1/baseline")
                ),
                (
                    CANDIDATE_CELL,
                    PathBuf::from("/home/eval/jobs/job-1/rounds/2/candidate")
                ),
            ]
        );
    }

    /// Task 3 review minor 4: an existing run is resumed only when it was
    /// frozen from the plan being executed.
    #[test]
    fn an_existing_run_frozen_from_another_plan_is_refused() {
        use crate::eval::CellSpec;
        let origin = origin();
        let (a, b) = (Path::new("a"), Path::new("b"));
        let plan = validation_plan("job-1", &origin, 1, 0, a, b);
        let digests = vec![
            "sha256:checkpoint".to_owned(),
            "sha256:candidate".to_owned(),
        ];
        let cell = |cell_id: &str, digest: &str| CellSpec {
            cell_id: cell_id.into(),
            label: cell_id.into(),
            subject: SubjectRef {
                pack_digest: digest.into(),
                behavior_id: "monitor".into(),
            },
            inference_profile_id: "local".into(),
        };
        let run = RunOrigin {
            definition: origin.definition.clone(),
            split: EvalSplit::Validation,
            case_ids: vec!["val-a".into(), "val-b".into()],
            cells: vec![
                cell(BASELINE_CELL, "sha256:checkpoint"),
                cell(CANDIDATE_CELL, "sha256:candidate"),
            ],
            trials_per_case: 2,
            seed_base: plan.seed_base,
            deadline_secs: Some(600),
            concurrency: 1,
            denominator_policy: String::new(),
            taxonomy_version: String::new(),
            max_infra_retries: 1,
            check_registry_version: String::new(),
            source_commit: String::new(),
            source_dirty: false,
            purpose: "optimization:job-1".into(),
            breaker_threshold: 5,
        };
        let captures = Some(origin.captures.as_slice());
        check_run_matches_plan(&run, captures, "job-1", &origin, &plan, &digests).unwrap();

        let other_candidate = vec!["sha256:checkpoint".to_owned(), "sha256:other".to_owned()];
        let error =
            check_run_matches_plan(&run, captures, "job-1", &origin, &plan, &other_candidate)
                .unwrap_err();
        assert!(
            job_refused(&error).unwrap().0.contains("cells"),
            "{error:#}"
        );

        // C1: a run frozen with another capture list, or with none recorded,
        // is not this job's run.
        for frozen in [Some(&[][..]), None] {
            let error = check_run_matches_plan(&run, frozen, "job-1", &origin, &plan, &digests)
                .unwrap_err();
            assert!(
                job_refused(&error).unwrap().0.contains("captures"),
                "{frozen:?}: {error:#}"
            );
        }

        let foreign = RunOrigin {
            purpose: "eval".into(),
            seed_base: plan.seed_base + 1,
            ..run
        };
        let error = check_run_matches_plan(&foreign, captures, "job-1", &origin, &plan, &digests)
            .unwrap_err();
        let refusal = &job_refused(&error).unwrap().0;
        assert!(
            refusal.contains("purpose") && refusal.contains("seed_base"),
            "{refusal}"
        );
    }

    async fn access() -> ConfigAccess {
        let node = std::sync::Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        ConfigAccess::Local(node)
    }

    /// Ruling P-N3: the decision and the exhaustion that ends the rounds are
    /// written together, so a resume never sees one without the other.
    #[tokio::test]
    async fn a_decision_and_the_exhaustion_it_causes_are_appended_together() {
        let access = access().await;
        let origin = origin();
        let mut job = create_job(&access, "job-1", &origin.owner, &origin)
            .await
            .unwrap();
        append(&access, &mut job, JournalEntry::Frozen)
            .await
            .unwrap();
        let pair = vec![
            JournalEntry::StructuralReject {
                round: 1,
                diagnostics: "empty_text: nothing".into(),
            },
            JournalEntry::BudgetExhausted {
                round: None,
                reason: "no budget".into(),
            },
        ];
        append_all(&access, &mut job, pair.clone()).await.unwrap();
        let stored = load_job(&access, &origin.owner, "job-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.journal.len(), 3);
        assert_eq!(stored.journal[1..], pair[..]);
        assert_eq!(stored.journal, job.journal);

        // A writer holding the old journal writes neither entry.
        let mut stale = JobRecord {
            journal: stored.journal[..1].to_vec(),
            ..stored
        };
        let error = append_all(&access, &mut stale, pair).await.unwrap_err();
        assert!(
            crate::optimization::job::journal_conflict(&error).is_some(),
            "{error:#}"
        );
        let after = load_job(&access, &origin.owner, "job-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.journal, job.journal);
    }

    /// Ruling P-N6: a bad job id is refused before anything is read, built or
    /// removed.
    #[tokio::test]
    async fn a_job_id_that_is_not_one_path_component_is_refused_before_any_write() {
        use crate::eval::runner::ScriptedExecutor;
        use crate::optimization::proposer::ScriptedProposer;
        let access = access().await;
        let home = tempfile::tempdir().unwrap();
        let mut bad = request();
        bad.job_id = "../escape".into();
        bad.jobs_dir = home.path().join("eval/jobs");
        let proposer = ScriptedProposer::new(Vec::new());
        let error = run_job(
            &access,
            &bad,
            &ScriptedExecutor::new(),
            &proposer,
            &CheckRegistry::builtin(),
            &PolicyV2::uncalibrated(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let refusal = job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(refusal.0.contains("job_id"), "{}", refusal.0);
        assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
        assert!(proposer.calls.lock().unwrap().is_empty());
    }
}
