pub mod embedded;
pub mod executor;
mod files;
pub mod freeze;
pub mod goal;
pub mod grade;
pub mod plan;
pub mod progress;
pub mod record;
pub mod review;
pub mod scripted;
pub mod view;

pub use executor::{
    Capture, CaptureResult, FileRef, FixtureDocument, FixtureFile, InferenceBinding, Isolation,
    StageEvidence, StageSpec, TrialEvidence, TrialExecutor, TrialFixtures, TrialLocator, TrialSpec,
};
pub use freeze::{
    freeze, freeze_refused, read_frozen_definition, CellRequest, CellSource, FreezeRefused,
    FrozenCell, FrozenRun, RunRequest, DEFINITION_FILE,
};
pub use goal::{case_goal, GoalEntry, SCHEMAS_GOAL};
pub use grade::{grade, VerdictRow};
pub use plan::{
    abandonment_bounded_slots, completion_is_not_evidence, not_evidence_slots, plan, trial_id_for,
    PlannedTrial, MAX_ABANDONED_ATTEMPTS,
};
pub use progress::{
    host_alive, is_fresh, read_progress, Holder, InFlight, LastToolCall, LiveSnapshot, Progress,
    StageProgress, ToolTally, PROGRESS_FILE,
};
pub use record::{DocumentRecorder, Recorder};
pub use scripted::{ScriptKey, ScriptedExecutor};
pub use view::{
    read_evidence_record, read_run_view, run_view, EvidenceRecord, RunView, EVIDENCE_FILE,
    REPORT_FILE,
};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::config_client::ConfigAccess;
use crate::document_config::{EvalCase, EvalFixtures, EvalSplit};
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::freeze::thaw;
use crate::eval::runner::progress::ProgressWriter;
use crate::eval::{
    OutcomeKind, RunRecord, StageCompletion, TrialCompletion, TrialIdentity, TrialRecord,
    VerdictDraft,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutcome {
    pub run_id: String,
    pub completed: u32,
    pub abandoned: u32,
    pub not_evidence: u32,
    pub breaker_tripped: bool,
    pub cancelled: bool,
}

#[derive(Debug)]
pub struct ProviderDown {
    pub outcome: RunOutcome,
    pub consecutive_not_evidence: u32,
}

impl std::fmt::Display for ProviderDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "eval run {} stopped: {} trials in a row produced no evidence",
            self.outcome.run_id, self.consecutive_not_evidence
        )
    }
}

impl std::error::Error for ProviderDown {}

pub fn provider_down(error: &anyhow::Error) -> Option<&ProviderDown> {
    error.downcast_ref::<ProviderDown>()
}

pub const CANCEL_MARKER: &str = "cancel";

pub fn run_dir(runs_dir: &Path, run_id: &str) -> Result<PathBuf> {
    freeze::directory_name("run_id", run_id)?;
    Ok(runs_dir.join(run_id))
}

pub fn request_cancel(runs_dir: &Path, run_id: &str) -> Result<PathBuf> {
    let dir = run_dir(runs_dir, run_id)?;
    if !dir.is_dir() {
        return Err(anyhow::Error::from(FreezeRefused(format!(
            "run {run_id} has no directory {}",
            dir.display()
        ))));
    }
    let marker = dir.join(CANCEL_MARKER);
    std::fs::write(&marker, b"").with_context(|| format!("writing {}", marker.display()))?;
    tracing::warn!(run_id, marker = %marker.display(), "eval run cancel requested");
    Ok(marker)
}

fn cancel_requested(run_dir: &Path, cancel: &CancellationToken) -> bool {
    if cancel.is_cancelled() {
        return true;
    }
    if run_dir.join(CANCEL_MARKER).exists() {
        tracing::warn!(
            run_dir = %run_dir.display(),
            "eval run cancel marker found; the run stops launching"
        );
        cancel.cancel();
        return true;
    }
    false
}

fn clear_cancel(run_dir: &Path) -> Result<()> {
    refuse_if_held(run_dir)?;
    let marker = run_dir.join(CANCEL_MARKER);
    match std::fs::remove_file(&marker) {
        Ok(()) => {
            tracing::info!(
                marker = %marker.display(),
                "eval run cancel marker cleared before planning"
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", marker.display())),
    }
}

fn refuse_if_held(run_dir: &Path) -> Result<()> {
    if running_elsewhere(run_dir) {
        return Err(anyhow::Error::from(FreezeRefused(format!(
            "run {} is still running in another process",
            run_dir.display()
        ))));
    }
    Ok(())
}

fn marker_poll(options: &RunOptions) -> Duration {
    options
        .poll_backoff_base
        .min(Duration::from_secs(MARKER_POLL_CAP_SECS))
        .max(Duration::from_millis(1))
}

const MARKER_POLL_CAP_SECS: u64 = 1;

pub const STALE_WINDOW: Duration = Duration::from_secs(3 * MARKER_POLL_CAP_SECS);

pub fn slots_owed(record: &RunRecord, trials: &[TrialRecord]) -> usize {
    plan(
        &record.origin,
        &record.run_id,
        trials,
        record.origin.max_infra_retries,
    )
    .len()
}

pub fn running_elsewhere(run_dir: &Path) -> bool {
    read_progress(run_dir).is_some_and(|progress| {
        progress
            .holder
            .as_ref()
            .is_some_and(|holder| holder.is_fresh(STALE_WINDOW))
            || progress
                .slots
                .values()
                .any(|slot| is_fresh(slot, STALE_WINDOW))
    })
}

pub fn run_finished(record: &RunRecord, trials: &[TrialRecord], run_dir: &Path) -> bool {
    slots_owed(record, trials) == 0 && !running_elsewhere(run_dir)
}

#[derive(Clone, Debug)]
pub struct TrialBudget(Arc<tokio::sync::Semaphore>);

impl TrialBudget {
    pub fn new(limit: usize) -> Result<Self> {
        anyhow::ensure!(
            limit > 0 && limit <= tokio::sync::Semaphore::MAX_PERMITS,
            "trial concurrency must be between 1 and {}",
            tokio::sync::Semaphore::MAX_PERMITS
        );
        Ok(Self(Arc::new(tokio::sync::Semaphore::new(limit))))
    }
}

impl PartialEq for TrialBudget {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for TrialBudget {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOptions {
    pub trial_budget: Option<TrialBudget>,
    pub poll_backoff_base: Duration,
    pub poll_backoff_cap: Duration,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            trial_budget: None,
            poll_backoff_base: Duration::from_secs(5),
            poll_backoff_cap: Duration::from_secs(60),
        }
    }
}

pub async fn prepare_run(
    access: &ConfigAccess,
    request: &RunRequest,
    isolation: Isolation,
) -> Result<FrozenRun> {
    refuse_if_held(&run_dir(&request.runs_dir, &request.run_id)?)?;
    freeze(access, request, isolation).await
}

pub async fn run(
    access: &ConfigAccess,
    request: &RunRequest,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: CancellationToken,
    options: &RunOptions,
) -> Result<RunOutcome> {
    let frozen = prepare_run(access, request, executor.isolation()).await?;
    clear_cancel(&frozen.run_dir)?;
    let recorder = DocumentRecorder::new(access);
    execute_frozen(&frozen, &recorder, executor, registry, cancel, options).await
}

#[allow(clippy::too_many_arguments)]
pub async fn resume(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
    runs_dir: &Path,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: CancellationToken,
    options: &RunOptions,
) -> Result<RunOutcome> {
    let frozen = thaw(access, owner, run_id, runs_dir, executor.isolation()).await?;
    clear_cancel(&frozen.run_dir)?;
    let recorder = DocumentRecorder::new(access);
    execute_frozen(&frozen, &recorder, executor, registry, cancel, options).await
}

enum Slot {
    Skipped,
    Abandoned,
    Completed { attempt: u32, not_evidence: bool },
}

pub(crate) async fn execute_frozen(
    frozen: &FrozenRun,
    recorder: &dyn Recorder,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: CancellationToken,
    options: &RunOptions,
) -> Result<RunOutcome> {
    let cancel = cancel.child_token();
    let owner = frozen.record.owner.as_str();
    let run_id = frozen.record.run_id.as_str();
    let origin = &frozen.record.origin;
    let concurrency = origin.concurrency.max(1) as usize;

    let mut outcome = RunOutcome {
        run_id: run_id.to_owned(),
        ..RunOutcome::default()
    };
    let mut consecutive = 0u32;
    let progress = ProgressWriter::new(&frozen.run_dir);
    let _held = progress.hold();
    let heartbeat_gap = marker_poll(options).max(Duration::from_millis(250));
    let views = ViewWriter::default();
    views.refresh(frozen, recorder).await;

    loop {
        progress.heartbeat(heartbeat_gap);
        if cancel_requested(&frozen.run_dir, &cancel) {
            break;
        }
        let existing = recorder.load_trials(owner, run_id).await?;
        let planned = plan(origin, run_id, &existing, origin.max_infra_retries);
        if planned.is_empty() {
            outcome.not_evidence = settled(run_id, &existing);
            break;
        }

        let stop = AtomicBool::new(false);
        let admission_stopped = CancellationToken::new();
        let mut retried: Vec<u32> = Vec::new();
        let mut tripped: Option<u32> = None;
        let mut failed: Option<anyhow::Error> = None;
        {
            let mut running = futures::stream::iter(planned.iter().map(|slot| {
                execute_trial(
                    frozen,
                    slot,
                    recorder,
                    executor,
                    registry,
                    &cancel,
                    &stop,
                    &progress,
                    &views,
                    options.trial_budget.as_ref(),
                    &admission_stopped,
                )
            }))
            .buffer_unordered(concurrency);

            let mut observed_cancel = false;
            let mut watch = tokio::time::interval(marker_poll(options));
            watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let next = if observed_cancel {
                    tokio::select! {
                        _ = watch.tick() => {
                            progress.heartbeat(heartbeat_gap);
                            continue;
                        }
                        next = running.next() => next,
                    }
                } else {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            observed_cancel = true;
                            stop.store(true, Ordering::Relaxed);
                            admission_stopped.cancel();
                            continue;
                        }
                        _ = watch.tick() => {
                            progress.heartbeat(heartbeat_gap);
                            cancel_requested(&frozen.run_dir, &cancel);
                            continue;
                        }
                        next = running.next() => next,
                    }
                };
                let slot = match next {
                    None => break,
                    Some(Ok(slot)) => slot,
                    Some(Err(error)) => {
                        tracing::error!(
                            run_id,
                            error = %format!("{error:#}"),
                            "eval trial failed; draining in-flight trials"
                        );
                        failed.get_or_insert(error);
                        stop.store(true, Ordering::Relaxed);
                        admission_stopped.cancel();
                        cancel.cancel();
                        continue;
                    }
                };
                match slot {
                    Slot::Skipped => {}
                    Slot::Abandoned => outcome.abandoned += 1,
                    Slot::Completed {
                        attempt,
                        not_evidence,
                    } => {
                        outcome.completed += 1;
                        if !not_evidence {
                            consecutive = 0;
                            continue;
                        }
                        retried.push(attempt);
                        consecutive += 1;
                        if consecutive >= frozen.breaker_threshold {
                            outcome.breaker_tripped = true;
                            tripped.get_or_insert(consecutive);
                            stop.store(true, Ordering::Relaxed);
                            admission_stopped.cancel();
                        }
                    }
                }
            }
        }

        if let Some(error) = failed {
            return Err(error);
        }
        if let Some(consecutive_not_evidence) = tripped {
            tracing::error!(
                run_id,
                consecutive_not_evidence,
                "eval run stopped: the provider is producing no evidence"
            );
            return Err(anyhow::Error::from(ProviderDown {
                outcome,
                consecutive_not_evidence,
            }));
        }
        if cancel_requested(&frozen.run_dir, &cancel) {
            break;
        }
        if let Some(backoff) = retried
            .iter()
            .map(|attempt| backoff_for(options, *attempt))
            .max()
        {
            tracing::info!(
                run_id,
                backoff = ?backoff,
                "eval run waits before planning the slots that produced no evidence again"
            );
            let wait = tokio::time::sleep(backoff);
            tokio::pin!(wait);
            let mut watch = tokio::time::interval(marker_poll(options));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = watch.tick() => {
                        progress.heartbeat(heartbeat_gap);
                        if cancel_requested(&frozen.run_dir, &cancel) {
                            break;
                        }
                    }
                    _ = &mut wait => break,
                }
            }
            if cancel.is_cancelled() {
                break;
            }
        }
    }

    if cancel.is_cancelled() {
        let existing = recorder.load_trials(owner, run_id).await?;
        if slots_owed(&frozen.record, &existing) == 0 {
            outcome.not_evidence = settled(run_id, &existing);
        } else {
            outcome.cancelled = true;
        }
    }
    views.refresh(frozen, recorder).await;
    tracing::info!(
        run_id,
        completed = outcome.completed,
        abandoned = outcome.abandoned,
        not_evidence = outcome.not_evidence,
        cancelled = outcome.cancelled,
        "eval run pass finished"
    );
    Ok(outcome)
}

fn settled(run_id: &str, existing: &[TrialRecord]) -> u32 {
    let bounded = abandonment_bounded_slots(existing);
    if bounded > 0 {
        tracing::warn!(
            run_id,
            slots = bounded,
            max_abandoned_attempts = MAX_ABANDONED_ATTEMPTS,
            "eval run stopped planning slots whose attempts were abandoned too often; they stay unanswered"
        );
    }
    not_evidence_slots(existing)
}

#[allow(clippy::too_many_arguments)]
async fn execute_trial(
    frozen: &FrozenRun,
    planned: &PlannedTrial,
    recorder: &dyn Recorder,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: &CancellationToken,
    stop: &AtomicBool,
    progress: &Arc<ProgressWriter>,
    views: &ViewWriter,
    trial_budget: Option<&TrialBudget>,
    admission_stopped: &CancellationToken,
) -> Result<Slot> {
    if stop.load(Ordering::Relaxed) || cancel_requested(&frozen.run_dir, cancel) {
        return Ok(Slot::Skipped);
    }
    let _permit = if let Some(budget) = trial_budget {
        let permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(Slot::Skipped),
            _ = admission_stopped.cancelled() => return Ok(Slot::Skipped),
            permit = budget.0.acquire() => permit.context("trial budget closed")?,
        };
        Some(permit)
    } else {
        None
    };
    if stop.load(Ordering::Relaxed) || cancel_requested(&frozen.run_dir, cancel) {
        return Ok(Slot::Skipped);
    }
    let owner = frozen.record.owner.as_str();
    let run_id = frozen.record.run_id.as_str();
    let case = frozen
        .definition
        .cases
        .iter()
        .find(|case| case.case_id == planned.case_id)
        .with_context(|| format!("run {run_id} froze no case {:?}", planned.case_id))?;
    let cell = frozen
        .cells
        .iter()
        .find(|cell| cell.spec.cell_id == planned.cell_id)
        .with_context(|| format!("run {run_id} froze no cell {:?}", planned.cell_id))?;
    let mut spec = trial_spec(frozen, planned, case, cell, executor.wants_script_key())?;
    let goal = case_goal(case, &frozen.captures);

    let locator = executor.provision(&spec).await;
    tracing::info!(
        target: "gents::eval",
        run_id,
        cell = %planned.cell_id,
        case = %planned.case_id,
        trial_index = planned.trial_index,
        attempt = planned.attempt,
        node_did = %locator.trial_node_did,
        session_id = %locator.session_id,
        "trial started"
    );
    let identity = TrialIdentity {
        trial_id: planned.trial_id.clone(),
        run_id: run_id.to_owned(),
        cell_id: planned.cell_id.clone(),
        case_id: planned.case_id.clone(),
        trial_index: planned.trial_index,
        attempt: planned.attempt,
        trial_node_did: locator.trial_node_did.clone(),
        session_id: locator.session_id.clone(),
        seed: planned.seed,
        home_hint: locator.home_hint.clone(),
    };
    if let Err(error) = recorder.create_trial(owner, &identity).await {
        executor.discard(&spec.trial_id).await;
        return Err(error);
    }
    progress.slot_started(
        &planned.trial_id,
        InFlight {
            cell_id: planned.cell_id.clone(),
            case_id: planned.case_id.clone(),
            trial_index: planned.trial_index,
            attempt: planned.attempt,
            stage_id: None,
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            pid: 0,
            written_at: String::new(),
            live: None,
            goal: goal.clone(),
        },
    );
    let _in_flight = InFlightGuard {
        progress,
        trial_id: &planned.trial_id,
    };
    spec.progress = StageProgress::for_trial(progress, &planned.trial_id);

    let evidence = executor.execute(&spec, cancel.child_token()).await;
    if cancel.is_cancelled() {
        tracing::info!(
            trial_id = %planned.trial_id,
            "eval trial abandoned on cancellation; its row stays open for a resume"
        );
        return Ok(Slot::Abandoned);
    }

    let split = frozen.record.origin.split;
    let rows = grade(case, &evidence, registry);
    for (index, row) in rows.iter().enumerate() {
        recorder
            .append_verdict(
                owner,
                split,
                &verdict_draft(run_id, &planned.trial_id, index, row, split),
            )
            .await?;
    }

    let completion = completion(case, &evidence);
    let not_evidence = completion_is_not_evidence(&completion);
    if not_evidence {
        tracing::warn!(
            trial_id = %planned.trial_id,
            attempt = planned.attempt,
            "eval trial learned nothing about its subject; the slot owes another attempt"
        );
    }
    recorder
        .complete_trial(owner, &planned.trial_id, &completion)
        .await?;
    write_evidence_sidecar(
        &spec.trial_dir,
        EvidenceRecord {
            evidence_digest: evidence.evidence_digest.clone(),
            anchor: evidence.anchor.clone(),
            ended_at: Some(completion.ended_at.clone()),
            live: progress.live_of(&planned.trial_id),
            goal,
        },
    );
    views.refresh(frozen, recorder).await;
    Ok(Slot::Completed {
        attempt: planned.attempt,
        not_evidence,
    })
}

struct InFlightGuard<'a> {
    progress: &'a ProgressWriter,
    trial_id: &'a str,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.progress.slot_ended(self.trial_id);
    }
}

fn write_evidence_sidecar(trial_dir: &Path, record: EvidenceRecord) {
    if trial_dir.as_os_str().is_empty() {
        return;
    }
    if let Err(error) = view::write_evidence_record(trial_dir, &record) {
        tracing::warn!(
            error = %format!("{error:#}"),
            "eval trial evidence digest was not recorded beside its home"
        );
    }
}

#[derive(Default)]
struct ViewWriter(tokio::sync::Mutex<()>);

impl ViewWriter {
    async fn refresh(&self, frozen: &FrozenRun, recorder: &dyn Recorder) {
        let _one = self.0.lock().await;
        let run_id = frozen.record.run_id.as_str();
        let runs_dir = frozen.run_dir.parent().unwrap_or(&frozen.run_dir);
        let report = match recorder
            .report(&frozen.record.owner, runs_dir, run_id)
            .await
        {
            Ok(Some(report)) => report,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(
                    run_id,
                    error = %format!("{error:#}"),
                    "eval run report was not derived for watchers"
                );
                return;
            }
        };
        if let Err(error) =
            view::write_run_view(&frozen.run_dir, &run_view(report, &frozen.run_dir))
        {
            tracing::warn!(
                run_id,
                error = %format!("{error:#}"),
                "eval run report was not written for watchers"
            );
        }
    }
}

fn trial_spec(
    frozen: &FrozenRun,
    planned: &PlannedTrial,
    case: &EvalCase,
    cell: &FrozenCell,
    wants_script_key: bool,
) -> Result<TrialSpec> {
    let mut inference = cell.inference.clone();
    inference.seed = planned.seed;
    Ok(TrialSpec {
        trial_id: planned.trial_id.clone(),
        pack_dir: cell.pack_dir.clone(),
        pack_digest: cell.spec.subject.pack_digest.clone(),
        agent_id: cell.spec.subject.agent_id.clone(),
        inference,
        fixtures: fixtures(
            &cell.pack_dir,
            frozen.definition.fixtures.as_ref(),
            case.fixtures.as_ref(),
        )?,
        host_bash: frozen.definition.subject.host_bash,
        stages: stage_specs(case, &frozen.captures),
        trial_dir: frozen.run_dir.join("trials").join(&planned.trial_id),
        script_key: wants_script_key.then(|| ScriptKey {
            cell_label: planned.cell_label.clone(),
            case_id: planned.case_id.clone(),
            trial_index: planned.trial_index,
            attempt: planned.attempt,
        }),
        progress: StageProgress::default(),
        review: review::StageReview::new(case),
    })
}

fn stage_specs(case: &EvalCase, fallback: &[Capture]) -> Vec<StageSpec> {
    case.stages
        .iter()
        .map(|stage| StageSpec {
            stage_id: stage.stage_id.clone(),
            prompt: stage.prompt.clone(),
            seed: stage.seed.as_ref().map(|seed| FixtureDocument {
                collection: seed.collection.clone(),
                document: seed.document.clone(),
            }),
            deadline_secs: stage.deadline_secs,
            settle: stage.settle,
            continuation: stage.continuation.clone(),
            review_previous: stage.review_previous,
            captures: if stage.capture.is_empty() {
                fallback.to_vec()
            } else {
                stage.capture.iter().map(Capture::from).collect()
            },
        })
        .collect()
}

fn fixtures(
    pack_dir: &Path,
    definition: Option<&EvalFixtures>,
    case: Option<&EvalFixtures>,
) -> Result<TrialFixtures> {
    let mut fixtures = TrialFixtures::default();
    for declared in [definition, case].into_iter().flatten() {
        fixtures.schemas.extend(declared.schemas.iter().cloned());
        fixtures
            .documents
            .extend(declared.documents.iter().map(|document| FixtureDocument {
                collection: document.collection.clone(),
                document: document.document.clone(),
            }));
        for asset in &declared.assets {
            fixtures.files.extend(asset_files(pack_dir, asset)?);
        }
        fixtures
            .files
            .extend(declared.files.iter().map(|file| FixtureFile {
                path: file.path.clone(),
                contents: file.contents.clone().into_bytes(),
            }));
    }
    Ok(fixtures)
}

fn asset_files(pack_dir: &Path, asset: &str) -> Result<Vec<FixtureFile>> {
    anyhow::ensure!(
        Path::new(asset)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "fixture asset {asset:?} must be a path inside the pack"
    );
    let path = pack_dir.join(asset);
    if !path.is_dir() {
        return Ok(vec![FixtureFile {
            path: asset.to_owned(),
            contents: std::fs::read(&path)
                .with_context(|| format!("reading fixture asset {}", path.display()))?,
        }]);
    }
    let mut files = Vec::new();
    let mut pending = vec![path];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading fixture asset directory {}", dir.display()))?
        {
            let entry = entry?;
            let entry_path = entry.path();
            if entry.file_type()?.is_dir() {
                pending.push(entry_path);
                continue;
            }
            let relative = entry_path
                .strip_prefix(pack_dir)
                .context("fixture asset outside the pack")?
                .to_str()
                .with_context(|| format!("{} is not UTF-8", entry_path.display()))?
                .to_owned();
            files.push(FixtureFile {
                path: relative,
                contents: std::fs::read(&entry_path)
                    .with_context(|| format!("reading fixture asset {}", entry_path.display()))?,
            });
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn verdict_draft(
    run_id: &str,
    trial_id: &str,
    index: usize,
    row: &VerdictRow,
    split: EvalSplit,
) -> VerdictDraft {
    VerdictDraft {
        verdict_id: verdict_id(trial_id, &row.stage_id, &row.check, index),
        run_id: run_id.to_owned(),
        trial_id: trial_id.to_owned(),
        stage_id: row.stage_id.clone(),
        check: row.check.clone(),
        check_version: row.check_version.clone(),
        tier: row.tier,
        kind: row.kind,
        provider_reason: row.provider_reason,
        score_bp: row.score_bp,
        weight: row.weight,
        raw: row.raw.clone(),
        feedback: (split == EvalSplit::Train)
            .then(|| row.feedback.clone())
            .flatten(),
        regrade_of: None,
    }
}

fn verdict_id(trial_id: &str, stage_id: &str, check: &str, index: usize) -> String {
    let material = format!(
        "{trial_id}
{stage_id}
{check}
{index}"
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

fn completion(case: &EvalCase, evidence: &TrialEvidence) -> TrialCompletion {
    let mut stages: Vec<StageCompletion> = evidence
        .stages
        .iter()
        .map(|stage| StageCompletion {
            stage_id: stage.stage_id.clone(),
            request_id: stage.request_id.clone(),
            terminal_state: stage.terminal_state,
            failure_kind: stage.failure_kind,
            provider_reason: stage.provider_reason,
        })
        .collect();
    for stage in &case.stages {
        if !evidence
            .stages
            .iter()
            .any(|observed| observed.stage_id == stage.stage_id)
        {
            stages.push(StageCompletion {
                stage_id: stage.stage_id.clone(),
                request_id: None,
                terminal_state: None,
                failure_kind: Some(OutcomeKind::SkippedPrerequisite),
                provider_reason: None,
            });
        }
    }
    TrialCompletion {
        ended_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        stages,
        usage: evidence.usage.clone(),
        anchor: evidence.anchor.clone(),
        evidence_digest: Some(evidence.evidence_digest.clone()),
    }
}

fn backoff_for(options: &RunOptions, attempt: u32) -> Duration {
    let factor = 1u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    options
        .poll_backoff_base
        .saturating_mul(factor)
        .min(options.poll_backoff_cap)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use anyhow::anyhow;
    use serde_json::{json, Value};

    use super::*;
    use crate::document_config::EvalFixtureDocument;
    use crate::document_config::EvalFixtureFile;
    use crate::eval::checks::{Check, CheckDescription, CheckVerdict};
    use crate::eval::runner::freeze::tests::{Launching, OWNER};
    use crate::eval::{
        invalidate_run, load_run, load_trials, load_verdicts, TrialRecord, VerdictRecord,
    };
    use crate::Collection;

    fn definition(check: &str) -> Value {
        json!({
            "definition_id": "loop-def",
            "node_did": OWNER,
            "comparability_version": 1,
            "subject": {"kind": "agent", "inference_slots": ["primary"]},
            "cases": [
                case("case-a", "validation", check),
                case("case-b", "validation", check),
                case("train-case", "train", check),
            ],
        })
    }

    fn case(case_id: &str, split: &str, check: &str) -> Value {
        json!({
            "case_id": case_id,
            "split": split,
            "stages": [{
                "stage_id": "check",
                "prompt": "Run the monitor.",
                "deadline_secs": 600,
                "checks": [{
                    "check": check,
                    "params": {"name": "findings", "min": 1},
                    "tier": "acceptance",
                }],
            }],
        })
    }

    async fn launching(check: &str) -> (Launching, PathBuf) {
        let launching = Launching::new().await;
        launching
            .install(vec![(Collection::EvalDefinition, definition(check))])
            .await;
        let pack = launching.pack("pack", "Off");
        (launching, pack)
    }

    fn request(launching: &Launching, pack: &Path, run_id: &str) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: OWNER.into(),
            evaluator_did: launching.evaluator_did(),
            definition_id: "loop-def".into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: vec![cell("base", pack), cell("cand", pack)],
            trials_per_case: 2,
            seed_base: 1000,
            deadline_secs: Some(600),
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            purpose: "eval".into(),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            runs_dir: launching.runs_dir(),
            captures: Vec::new(),
        }
    }

    fn one_slot(request: &mut RunRequest) {
        request.cells.truncate(1);
        request.case_ids = Some(vec!["case-a".into()]);
        request.trials_per_case = 1;
    }

    fn cell(cell_id: &str, pack: &Path) -> CellRequest {
        CellRequest {
            cell_id: cell_id.into(),
            label: cell_id.into(),
            source: CellSource::Directory(pack.to_path_buf()),
            agent_id: "monitor".into(),
            inference_profile_id: "local".into(),
        }
    }

    fn options() -> RunOptions {
        RunOptions {
            trial_budget: None,
            poll_backoff_base: Duration::from_millis(1),
            poll_backoff_cap: Duration::from_millis(2),
        }
    }

    fn passed() -> TrialEvidence {
        ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
    }

    fn key(case_id: &str, attempt: u32) -> ScriptKey {
        ScriptKey {
            cell_label: "base".into(),
            case_id: case_id.into(),
            trial_index: 0,
            attempt,
        }
    }

    struct BudgetExecutor {
        started: tokio::sync::mpsc::UnboundedSender<(String, tokio::sync::oneshot::Sender<()>)>,
        active: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for BudgetExecutor {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }
        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }
        async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            let (send, receive) = tokio::sync::oneshot::channel();
            self.started.send((spec.trial_id.clone(), send)).unwrap();
            receive.await.unwrap();
            self.active.fetch_sub(1, Ordering::SeqCst);
            passed()
        }
        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    #[tokio::test]
    async fn shared_trial_budget_reuses_capacity_across_runs_without_exceeding_ceiling() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut a = request(&launching, &pack, "budget-a");
        one_slot(&mut a);
        a.trials_per_case = 2;
        let mut b = a.clone();
        b.run_id = "budget-b".into();
        let a = prepare_run(&launching.access, &a, Isolation::Embedded)
            .await
            .unwrap();
        let b = prepare_run(&launching.access, &b, Isolation::Embedded)
            .await
            .unwrap();
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let executor = BudgetExecutor {
            started,
            active: 0.into(),
            peak: 0.into(),
        };
        let recorder = DocumentRecorder::new(&launching.access);
        let registry = CheckRegistry::builtin();
        let mut opts = options();
        opts.trial_budget = Some(TrialBudget::new(2).unwrap());
        let observe = async {
            let first = starts.recv().await.unwrap();
            let second = starts.recv().await.unwrap();
            assert_eq!(executor.active.load(Ordering::SeqCst), 2);
            first.1.send(()).unwrap();
            let third = starts.recv().await.unwrap();
            assert_eq!(
                executor.active.load(Ordering::SeqCst),
                2,
                "freed capacity is used while another run remains active"
            );
            third.1.send(()).unwrap();
            second.1.send(()).unwrap();
            starts.recv().await.unwrap().1.send(()).unwrap();
        };
        let (a_result, b_result, ()) = tokio::join!(
            execute_frozen(
                &a,
                &recorder,
                &executor,
                &registry,
                CancellationToken::new(),
                &opts
            ),
            execute_frozen(
                &b,
                &recorder,
                &executor,
                &registry,
                CancellationToken::new(),
                &opts
            ),
            observe,
        );
        assert_eq!(a_result.unwrap().completed, 2);
        assert_eq!(b_result.unwrap().completed, 2);
        assert_eq!(executor.peak.load(Ordering::SeqCst), 2);
        assert_eq!(opts.trial_budget.as_ref().unwrap().0.available_permits(), 2);
    }

    #[tokio::test]
    async fn cancelled_budget_wait_creates_no_attempt_and_does_not_leak_capacity() {
        assert!(TrialBudget::new(0).is_err());
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "budget-cancelled");
        one_slot(&mut request);
        let frozen = prepare_run(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let budget = TrialBudget::new(1).unwrap();
        let held = budget.0.acquire().await.unwrap();
        let cancel = CancellationToken::new();
        let executor = ScriptedExecutor::new().with_default(passed());
        let recorder = DocumentRecorder::new(&launching.access);
        let registry = CheckRegistry::builtin();
        let progress = ProgressWriter::new(&frozen.run_dir);
        let views = ViewWriter::default();
        let stop = AtomicBool::new(false);
        let admission_stopped = CancellationToken::new();
        let planned = plan(
            &frozen.record.origin,
            &request.run_id,
            &[],
            request.max_infra_retries,
        );
        let trial = execute_trial(
            &frozen,
            &planned[0],
            &recorder,
            &executor,
            &registry,
            &cancel,
            &stop,
            &progress,
            &views,
            Some(&budget),
            &admission_stopped,
        );
        tokio::pin!(trial);
        assert!(futures::poll!(&mut trial).is_pending());
        cancel.cancel();
        assert!(matches!(trial.await.unwrap(), Slot::Skipped));
        assert!(recorder
            .load_trials(OWNER, &request.run_id)
            .await
            .unwrap()
            .is_empty());
        drop(held);
        assert_eq!(budget.0.available_permits(), 1);
        let mut opts = options();
        opts.trial_budget = Some(budget.clone());
        let outcome = execute_frozen(
            &frozen,
            &recorder,
            &executor,
            &registry,
            CancellationToken::new(),
            &opts,
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 1);
        assert_eq!(outcome.abandoned, 0);
        assert_eq!(budget.0.available_permits(), 1);
    }

    #[tokio::test]
    async fn stopped_budget_wait_finishes_without_waiting_for_unrelated_capacity() {
        assert!(TrialBudget::new(0).is_err());
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "budget-cancelled");
        one_slot(&mut request);
        let frozen = prepare_run(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let budget = TrialBudget::new(1).unwrap();
        let held = budget.0.acquire().await.unwrap();
        let cancel = CancellationToken::new();
        let executor = ScriptedExecutor::new().with_default(passed());
        let recorder = DocumentRecorder::new(&launching.access);
        let registry = CheckRegistry::builtin();
        let progress = ProgressWriter::new(&frozen.run_dir);
        let views = ViewWriter::default();
        let stop = AtomicBool::new(false);
        let admission_stopped = CancellationToken::new();
        let planned = plan(
            &frozen.record.origin,
            &request.run_id,
            &[],
            request.max_infra_retries,
        );
        let trial = execute_trial(
            &frozen,
            &planned[0],
            &recorder,
            &executor,
            &registry,
            &cancel,
            &stop,
            &progress,
            &views,
            Some(&budget),
            &admission_stopped,
        );
        tokio::pin!(trial);
        assert!(futures::poll!(&mut trial).is_pending());
        stop.store(true, Ordering::Relaxed);
        admission_stopped.cancel();
        assert!(matches!(
            futures::poll!(&mut trial),
            std::task::Poll::Ready(Ok(Slot::Skipped))
        ));
        assert!(recorder
            .load_trials(OWNER, &request.run_id)
            .await
            .unwrap()
            .is_empty());
        drop(held);
        assert_eq!(budget.0.available_permits(), 1);
        let mut opts = options();
        opts.trial_budget = Some(budget.clone());
        let outcome = execute_frozen(
            &frozen,
            &recorder,
            &executor,
            &registry,
            CancellationToken::new(),
            &opts,
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 1);
        assert_eq!(outcome.abandoned, 0);
        assert_eq!(budget.0.available_permits(), 1);
    }

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
                feedback: Some("more rows next time".into()),
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

    struct CancelOnFirstExecute {
        run: CancellationToken,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for CancelOnFirstExecute {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
            self.run.cancel();
            cancel.cancelled().await;
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    struct BreakerExecutor {
        gate: CancellationToken,
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
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            let index = spec.script_key.as_ref().map_or(0, |key| key.trial_index);
            if index == 0 {
                return ScriptedExecutor::not_evidence("did:key:x");
            }
            self.gate.cancelled().await;
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    struct GatingRecorder<'a> {
        inner: DocumentRecorder<'a>,
        completions: Mutex<u32>,
        open_after: u32,
        gate: CancellationToken,
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

    #[derive(Default)]
    struct SettlesOnCancel {
        settled: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for SettlesOnCancel {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
            cancel.cancelled().await;
            self.settled.fetch_add(1, Ordering::SeqCst);
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    struct DiscardingExecutor {
        inner: ScriptedExecutor,
        discarded: Mutex<Vec<String>>,
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

        async fn recollect(
            &self,
            at: &TrialLocator,
            captures: &[Capture],
        ) -> Option<TrialEvidence> {
            self.inner.recollect(at, captures).await
        }
    }

    struct FaultingRecorder<'a> {
        inner: DocumentRecorder<'a>,
        fail_on: Option<(&'static str, u32)>,
        calls: Mutex<Vec<(&'static str, String)>>,
    }

    impl<'a> FaultingRecorder<'a> {
        fn new(access: &'a ConfigAccess) -> Self {
            Self {
                inner: DocumentRecorder::new(access),
                fail_on: None,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn failing(mut self, method: &'static str, nth: u32) -> Self {
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

        fn log(&self) -> Vec<(&'static str, String)> {
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

    fn completion_of(trials: &[TrialRecord], attempt: u32) -> TrialCompletion {
        trials
            .iter()
            .find(|trial| trial.identity.attempt == attempt)
            .and_then(|trial| trial.completion.clone())
            .unwrap_or_else(|| panic!("a completion for attempt {attempt}"))
    }

    fn trial_keys(trials: &[TrialRecord]) -> Vec<(String, String, u32, u32, bool)> {
        let mut keys: Vec<_> = trials
            .iter()
            .map(|trial| {
                (
                    trial.identity.cell_id.clone(),
                    trial.identity.case_id.clone(),
                    trial.identity.trial_index,
                    trial.identity.attempt,
                    trial.completion.is_some(),
                )
            })
            .collect();
        keys.sort();
        keys
    }

    fn verdict_keys(
        verdicts: &[VerdictRecord],
    ) -> Vec<(String, String, String, &'static str, Option<u32>)> {
        let mut keys: Vec<_> = verdicts
            .iter()
            .map(|verdict| {
                (
                    verdict.trial_id.clone(),
                    verdict.stage_id.clone(),
                    verdict.check.clone(),
                    verdict.kind.as_str(),
                    verdict.score_bp,
                )
            })
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn fixtures_merge_the_case_after_the_definition_and_read_assets_from_the_pack() {
        let pack = tempfile::tempdir().unwrap();
        std::fs::write(pack.path().join("seed.json"), b"{}").unwrap();
        let shared = EvalFixtures {
            assets: vec!["seed.json".into()],
            documents: Vec::new(),
            files: Vec::new(),
            schemas: vec!["type A {}".into()],
        };
        let per_case = EvalFixtures {
            assets: Vec::new(),
            documents: vec![EvalFixtureDocument {
                collection: "A".into(),
                document: json!({"id": 1}),
            }],
            files: vec![EvalFixtureFile {
                path: "inventory/edge-07.json".into(),
                contents: "{\"disk\": \"84%\"}\n".into(),
            }],
            schemas: vec!["type B {}".into()],
        };

        let merged = fixtures(pack.path(), Some(&shared), Some(&per_case)).unwrap();
        assert_eq!(merged.schemas, ["type A {}", "type B {}"]);
        assert_eq!(merged.documents.len(), 1);
        assert_eq!(
            merged.files,
            [
                FixtureFile {
                    path: "seed.json".into(),
                    contents: b"{}".to_vec(),
                },
                FixtureFile {
                    path: "inventory/edge-07.json".into(),
                    contents: b"{\"disk\": \"84%\"}\n".to_vec(),
                },
            ]
        );

        let escaping = EvalFixtures {
            assets: vec!["../outside.json".into()],
            ..EvalFixtures::default()
        };
        let error = fixtures(pack.path(), Some(&escaping), None).unwrap_err();
        assert!(
            format!("{error:#}").contains("inside the pack"),
            "{error:#}"
        );

        let absent = EvalFixtures {
            assets: vec!["gone.json".into()],
            ..EvalFixtures::default()
        };
        assert!(
            fixtures(pack.path(), Some(&absent), None).is_err(),
            "a missing asset is the runner's failure, not a trial outcome"
        );
    }

    #[test]
    fn a_directory_asset_materializes_every_file_under_it() {
        let pack = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(pack.path().join("brief/notes")).unwrap();
        std::fs::write(pack.path().join("brief/kickoff.md"), b"k").unwrap();
        std::fs::write(pack.path().join("brief/notes/spec.json"), b"{}").unwrap();
        std::fs::write(pack.path().join("other.md"), b"o").unwrap();
        let declared = EvalFixtures {
            assets: vec!["brief".into()],
            ..EvalFixtures::default()
        };
        let merged = fixtures(pack.path(), Some(&declared), None).unwrap();
        assert_eq!(
            merged.files,
            [
                FixtureFile {
                    path: "brief/kickoff.md".into(),
                    contents: b"k".to_vec(),
                },
                FixtureFile {
                    path: "brief/notes/spec.json".into(),
                    contents: b"{}".to_vec(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_two_cell_run_completes_every_slot_and_writes_verdicts_before_completion() {
        let (launching, pack) = launching("captured_rows_count").await;
        let request = request(&launching, &pack, "run-1");
        let executor = ScriptedExecutor::new().with_default(passed());
        let recorder = FaultingRecorder::new(&launching.access);

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let outcome = execute_frozen(
            &frozen,
            &recorder,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            RunOutcome {
                run_id: "run-1".into(),
                completed: 8,
                abandoned: 0,
                not_evidence: 0,
                breaker_tripped: false,
                cancelled: false,
            }
        );

        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(trials.len(), 8);
        assert!(trials.iter().all(|trial| trial.completion.is_some()));
        assert!(
            trials
                .iter()
                .all(|trial| trial.identity.seed == 1000 + trial.identity.trial_index as i64),
            "each trial index is seeded from the run's seed base"
        );
        let completion = trials[0].completion.as_ref().expect("a completion");
        assert_eq!(completion.stages.len(), 1);
        assert_eq!(completion.stages[0].stage_id, "check");
        assert_eq!(completion.stages[0].failure_kind, None);

        let verdicts = load_verdicts(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(verdicts.len(), 8);
        assert!(verdicts
            .iter()
            .all(|verdict| verdict.kind == OutcomeKind::Passed && verdict.feedback.is_none()));

        for trial in &trials {
            let path = frozen
                .run_dir
                .join("trials")
                .join(&trial.identity.trial_id)
                .join("evidence.json");
            let recorded: Value =
                serde_json::from_slice(&std::fs::read(&path).expect("an evidence record"))
                    .expect("the evidence record parses");
            assert_eq!(recorded["evidence_digest"], passed().evidence_digest);
            assert_eq!(recorded["anchor"]["requests"], 1, "{recorded}");
        }

        let calls = recorder.log();
        for trial in &trials {
            let id = &trial.identity.trial_id;
            let position = |method: &str| {
                calls
                    .iter()
                    .position(|(name, subject)| *name == method && subject == id)
                    .unwrap_or_else(|| panic!("no {method} call for {id}"))
            };
            assert!(
                position("create_trial") < position("append_verdict"),
                "the row exists before its verdicts"
            );
            assert!(
                position("append_verdict") < position("complete_trial"),
                "a trial's verdicts are written before its completion"
            );
        }
    }

    #[tokio::test]
    async fn a_crash_after_create_trial_is_repaired_by_resume() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        one_slot(&mut request);
        let executor = ScriptedExecutor::new().with_default(passed());
        let registry = CheckRegistry::builtin();
        let recorder = FaultingRecorder::new(&launching.access).failing("complete_trial", 1);

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let error = execute_frozen(
            &frozen,
            &recorder,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected"), "{error:#}");

        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(
            trial_keys(&trials),
            [("base".into(), "case-a".into(), 0, 1, false)]
        );
        let verdicts = load_verdicts(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].trial_id, trials[0].identity.trial_id);

        let outcome = resume(
            &launching.access,
            OWNER,
            "run-1",
            &launching.runs_dir(),
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 1);

        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(
            trial_keys(&trials),
            [
                ("base".to_string(), "case-a".to_string(), 0, 1, false),
                ("base".to_string(), "case-a".to_string(), 0, 2, true),
            ],
            "the abandoned row stays, and the repaired attempt completes"
        );
    }

    #[tokio::test]
    async fn a_trial_whose_row_cannot_be_written_is_handed_back_to_the_executor() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        one_slot(&mut request);
        let executor = DiscardingExecutor {
            inner: ScriptedExecutor::new().with_default(passed()),
            discarded: Mutex::new(Vec::new()),
        };
        let recorder = FaultingRecorder::new(&launching.access).failing("create_trial", 1);

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let error = execute_frozen(
            &frozen,
            &recorder,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected"), "{error:#}");

        let owed = plan(&frozen.record.origin, "run-1", &[], 1);
        assert_eq!(owed.len(), 1);
        assert_eq!(
            executor
                .discarded
                .lock()
                .expect("the discard log")
                .as_slice(),
            [owed[0].trial_id.clone()],
            "the provisioned trial was released"
        );
        assert!(
            executor
                .inner
                .calls
                .lock()
                .expect("the execute log")
                .is_empty(),
            "a trial whose row was never written is not run"
        );
        assert!(
            load_trials(&launching.access, OWNER, "run-1")
                .await
                .unwrap()
                .is_empty(),
            "the row the loop failed to write is not there"
        );
    }

    #[tokio::test]
    async fn not_evidence_retries_up_to_the_cap_then_the_slot_is_left_as_not_evidence() {
        let (launching, pack) = launching("captured_rows_count").await;
        let registry = CheckRegistry::builtin();

        let mut retrying = request(&launching, &pack, "run-1");
        one_slot(&mut retrying);
        retrying.max_infra_retries = 3;
        let executor = ScriptedExecutor::new()
            .with(
                key("case-a", 1),
                ScriptedExecutor::not_evidence("did:key:x"),
            )
            .with(
                key("case-a", 2),
                ScriptedExecutor::not_evidence("did:key:x"),
            )
            .with(key("case-a", 3), passed());
        let outcome = run(
            &launching.access,
            &retrying,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.not_evidence), (3, 0));
        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(
            trial_keys(&trials),
            [
                ("base".to_string(), "case-a".to_string(), 0, 1, true),
                ("base".to_string(), "case-a".to_string(), 0, 2, true),
                ("base".to_string(), "case-a".to_string(), 0, 3, true),
            ],
            "an attempt that learned nothing still finished, so it is completed"
        );
        let empty = completion_of(&trials, 1);
        assert!(
            completion_is_not_evidence(&empty),
            "the attempt that never reached its stage is not evidence"
        );
        assert_eq!(empty.anchor.requests, 0);
        assert!(
            !completion_is_not_evidence(&completion_of(&trials, 3)),
            "the attempt that ran its stage is"
        );

        let mut exhausting = request(&launching, &pack, "run-2");
        one_slot(&mut exhausting);
        exhausting.max_infra_retries = 3;
        let executor =
            ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:x"));
        let outcome = run(
            &launching.access,
            &exhausting,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.not_evidence), (4, 1));
        let trials = load_trials(&launching.access, OWNER, "run-2")
            .await
            .unwrap();
        assert_eq!(
            trials.iter().map(|t| t.identity.attempt).max(),
            Some(4),
            "the slot stops at max_infra_retries + 1 attempts"
        );
        assert!(trials.iter().all(|trial| trial.completion.is_some()));
        assert!(
            completion_is_not_evidence(&completion_of(&trials, 4)),
            "the slot's latest completed attempt is still not evidence"
        );
        assert_eq!(not_evidence_slots(&trials), 1);
    }

    #[tokio::test]
    async fn a_stage_that_failed_on_the_harness_re_plans_the_slot() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        one_slot(&mut request);
        request.max_infra_retries = 1;

        let executor = ScriptedExecutor::new()
            .with(
                key("case-a", 1),
                ScriptedExecutor::failed_evidence(
                    "did:key:trial",
                    "check",
                    OutcomeKind::Infrastructure,
                    None,
                ),
            )
            .with(key("case-a", 2), passed());
        let outcome = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();

        assert_eq!((outcome.completed, outcome.not_evidence), (2, 0));
        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(
            trial_keys(&trials),
            [
                ("base".to_string(), "case-a".to_string(), 0, 1, true),
                ("base".to_string(), "case-a".to_string(), 0, 2, true),
            ],
            "the first attempt learned nothing, so the slot owed another"
        );
        assert!(completion_is_not_evidence(&completion_of(&trials, 1)));

        let verdicts = load_verdicts(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        let first = trials
            .iter()
            .find(|trial| trial.identity.attempt == 1)
            .expect("the first attempt");
        let rows = verdicts
            .iter()
            .filter(|verdict| verdict.trial_id == first.identity.trial_id)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].kind, rows[0].score_bp),
            (OutcomeKind::Infrastructure, None)
        );
    }

    #[tokio::test]
    async fn the_breaker_trips_on_consecutive_not_evidence_and_resume_continues() {
        let (launching, pack) = launching("captured_rows_count").await;
        let registry = CheckRegistry::builtin();
        let mut request = request(&launching, &pack, "run-1");
        request.cells.truncate(1);
        request.trials_per_case = 1;
        request.max_infra_retries = 3;
        request.breaker_threshold = 2;

        let executor =
            ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:x"));
        let error = run(
            &launching.access,
            &request,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        let down =
            provider_down(&error).unwrap_or_else(|| panic!("expected ProviderDown: {error:#}"));
        assert_eq!(
            (down.outcome.run_id.as_str(), down.consecutive_not_evidence),
            ("run-1", 2)
        );
        assert!(down.outcome.breaker_tripped);
        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(trials.len(), 2);
        assert!(trials.iter().all(|trial| trial
            .completion
            .as_ref()
            .is_some_and(completion_is_not_evidence)));

        let recovered = ScriptedExecutor::new().with_default(passed());
        let outcome = resume(
            &launching.access,
            OWNER,
            "run-1",
            &launching.runs_dir(),
            &recovered,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 2);
        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(
            trial_keys(&trials),
            [
                ("base".to_string(), "case-a".to_string(), 0, 1, true),
                ("base".to_string(), "case-a".to_string(), 0, 2, true),
                ("base".to_string(), "case-b".to_string(), 0, 1, true),
                ("base".to_string(), "case-b".to_string(), 0, 2, true),
            ]
        );
        assert_eq!(
            not_evidence_slots(&trials),
            0,
            "both slots learned something"
        );
    }

    #[tokio::test]
    async fn resume_refuses_an_embedded_executor_for_a_pack_that_grants_host_bash() {
        let (launching, _) = launching("captured_rows_count").await;
        let pack = launching.pack("unrestricted", "Unrestricted");
        let mut request = request(&launching, &pack, "run-1");
        one_slot(&mut request);
        freeze(&launching.access, &request, Isolation::Process)
            .await
            .unwrap();

        let error = resume(
            &launching.access,
            OWNER,
            "run-1",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        let reason = freeze_refused(&error)
            .unwrap_or_else(|| panic!("expected a FreezeRefused, got {error:#}"));
        assert!(
            reason.0.contains("monitor-tools") && reason.0.contains("Unrestricted"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn the_breaker_reports_the_count_it_tripped_on_not_the_one_it_ended_with() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        request.cells.truncate(1);
        request.concurrency = 4;
        request.breaker_threshold = 2;
        let gate = CancellationToken::new();
        let recorder = GatingRecorder {
            inner: DocumentRecorder::new(&launching.access),
            completions: Mutex::new(0),
            open_after: 2,
            gate: gate.clone(),
        };

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let error = execute_frozen(
            &frozen,
            &recorder,
            &BreakerExecutor { gate },
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();

        let down = provider_down(&error)
            .unwrap_or_else(|| panic!("expected a ProviderDown, got {error:#}"));
        assert_eq!(down.consecutive_not_evidence, 2);
        assert!(down.outcome.breaker_tripped);
        assert_eq!(
            down.outcome.completed, 4,
            "the trials already in flight at the trip are recorded like any other"
        );
        assert!(format!("{down}").contains("2 trials in a row"), "{down}");
        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(
            not_evidence_slots(&trials),
            2,
            "the two empty slots, once each"
        );
    }

    #[tokio::test]
    async fn cancel_stops_launching_and_leaves_in_flight_rows_null() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        request.cells.truncate(1);
        request.trials_per_case = 1;

        let cancel = CancellationToken::new();
        let executor = CancelOnFirstExecute {
            run: cancel.clone(),
        };
        let outcome = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            cancel,
            &options(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.abandoned), (0, 1));

        let trials = load_trials(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(trials.len(), 1, "the second slot is never launched");
        assert!(trials[0].completion.is_none());
        assert!(load_verdicts(&launching.access, OWNER, "run-1")
            .await
            .unwrap()
            .is_empty());
    }

    struct MarkOnFirstExecute {
        inner: ScriptedExecutor,
        runs_dir: PathBuf,
        run_id: String,
        marked: AtomicBool,
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

        async fn recollect(
            &self,
            at: &TrialLocator,
            captures: &[Capture],
        ) -> Option<TrialEvidence> {
            self.inner.recollect(at, captures).await
        }
    }

    #[tokio::test]
    async fn a_cancel_marker_stops_the_loop_at_its_next_launch_and_resume_clears_it() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-marker");
        request.cells.truncate(1);
        request.trials_per_case = 1;
        let caller = CancellationToken::new();
        let executor = MarkOnFirstExecute {
            inner: ScriptedExecutor::new().with_default(passed()),
            runs_dir: launching.runs_dir(),
            run_id: "run-marker".into(),
            marked: AtomicBool::new(false),
        };
        let outcome = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            caller.clone(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(
            (outcome.completed, outcome.abandoned),
            (1, 0),
            "the trial that was running finishes; the next is never launched"
        );
        assert!(outcome.cancelled, "the pass says why it stopped");
        assert!(
            !caller.is_cancelled(),
            "the marker stops this run, never the caller's other work"
        );
        let marker = launching.runs_dir().join("run-marker").join(CANCEL_MARKER);
        assert!(marker.exists());
        assert_eq!(
            load_trials(&launching.access, OWNER, "run-marker")
                .await
                .unwrap()
                .len(),
            1
        );

        let resumed = resume(
            &launching.access,
            OWNER,
            "run-marker",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(resumed.completed, 1);
        assert!(
            !marker.exists(),
            "resume removes the marker before it plans"
        );
        assert_eq!(
            load_trials(&launching.access, OWNER, "run-marker")
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_marker_written_before_the_loop_starts_launches_nothing() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-early");
        one_slot(&mut request);
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        request_cancel(&launching.runs_dir(), "run-early").unwrap();
        let outcome = execute_frozen(
            &frozen,
            &DocumentRecorder::new(&launching.access),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            RunOutcome {
                run_id: "run-early".into(),
                cancelled: true,
                ..RunOutcome::default()
            }
        );
        assert!(load_trials(&launching.access, OWNER, "run-early")
            .await
            .unwrap()
            .is_empty());
    }

    struct MarkAfterComplete<'a> {
        inner: DocumentRecorder<'a>,
        runs_dir: PathBuf,
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

    #[tokio::test]
    async fn a_marker_written_after_the_last_completion_is_not_a_cancelled_run() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-late");
        one_slot(&mut request);
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let recorder = MarkAfterComplete {
            inner: DocumentRecorder::new(&launching.access),
            runs_dir: launching.runs_dir(),
        };
        let outcome = execute_frozen(
            &frozen,
            &recorder,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert!(
            launching
                .runs_dir()
                .join("run-late")
                .join(CANCEL_MARKER)
                .exists(),
            "the marker landed"
        );
        assert_eq!(outcome.completed, 1);
        assert!(!outcome.cancelled, "the run owes nothing: {outcome:?}");
    }

    #[tokio::test]
    async fn run_on_an_existing_run_id_clears_the_marker_like_resume() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-again");
        one_slot(&mut request);
        freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let marker = request_cancel(&launching.runs_dir(), "run-again").unwrap();
        let outcome = run(
            &launching.access,
            &request,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 1);
        assert!(!outcome.cancelled);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn an_error_in_one_trial_drains_the_trials_in_flight() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-drain");
        request.concurrency = 2;
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let executor = SettlesOnCancel::default();
        let recorder = FaultingRecorder::new(&launching.access).failing("create_trial", 2);
        let error = execute_frozen(
            &frozen,
            &recorder,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected"), "{error:#}");
        assert_eq!(
            executor.settled.load(Ordering::SeqCst),
            1,
            "the trial in flight saw the cancellation and wound down"
        );
    }

    #[tokio::test]
    async fn run_and_resume_refuse_a_run_held_by_a_live_process() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-held");
        one_slot(&mut request);
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let holder = ProgressWriter::new(&frozen.run_dir);
        let _held = holder.hold();
        let marker = request_cancel(&launching.runs_dir(), "run-held").unwrap();
        let definition = frozen.run_dir.join(freeze::DEFINITION_FILE);
        let long_ago = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(&definition)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        holder.heartbeat(Duration::ZERO);
        let ran = run(
            &launching.access,
            &request,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        holder.heartbeat(Duration::ZERO);
        let resumed = resume(
            &launching.access,
            OWNER,
            "run-held",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        for error in [ran, resumed] {
            assert!(
                freeze_refused(&error).is_some_and(|refusal| refusal.0.contains("still running")),
                "{error:#}"
            );
        }
        holder.heartbeat(Duration::ZERO);
        assert!(marker.exists(), "the holder's cancel request survives");
        assert_eq!(
            std::fs::metadata(&definition).unwrap().modified().unwrap(),
            long_ago,
            "run refuses before freezing rewrites the run's files"
        );
        assert!(running_elsewhere(&frozen.run_dir));
        assert!(load_trials(&launching.access, OWNER, "run-held")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn request_cancel_refuses_a_missing_run_directory_and_a_path_for_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let missing = request_cancel(dir.path(), "absent").unwrap_err();
        assert!(
            freeze_refused(&missing).is_some_and(|refusal| refusal.0.contains("has no directory")),
            "{missing:#}"
        );
        let escaping = request_cancel(dir.path(), "../elsewhere").unwrap_err();
        assert!(
            freeze_refused(&escaping)
                .is_some_and(|refusal| refusal.0.contains("one ordinary path component")),
            "{escaping:#}"
        );
        assert_eq!(
            run_dir(dir.path(), "run-1").unwrap(),
            dir.path().join("run-1")
        );
    }

    #[tokio::test]
    async fn an_invalidated_run_refuses_resume() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-1");
        one_slot(&mut request);
        freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        invalidate_run(&launching.access, OWNER, "run-1", OWNER, "grader bug")
            .await
            .unwrap();

        let error = resume(
            &launching.access,
            OWNER,
            "run-1",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        let reason = format!("{error:#}");
        assert!(
            reason.contains("run-1") && reason.contains("invalidated"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn feedback_is_dropped_off_train() {
        let (launching, pack) = launching("feedback_check").await;
        let registry = CheckRegistry::builtin().with(Box::new(FeedbackCheck));
        let executor = ScriptedExecutor::new().with_default(passed());

        let mut off_train = request(&launching, &pack, "run-1");
        one_slot(&mut off_train);
        run(
            &launching.access,
            &off_train,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let verdicts = load_verdicts(&launching.access, OWNER, "run-1")
            .await
            .unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].check, "feedback_check");
        assert_eq!(verdicts[0].feedback, None, "validation keeps no feedback");

        let mut on_train = request(&launching, &pack, "run-2");
        on_train.cells.truncate(1);
        on_train.split = EvalSplit::Train;
        on_train.case_ids = Some(vec!["train-case".into()]);
        on_train.trials_per_case = 1;
        run(
            &launching.access,
            &on_train,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let verdicts = load_verdicts(&launching.access, OWNER, "run-2")
            .await
            .unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].feedback.as_deref(), Some("more rows next time"));
    }

    #[tokio::test]
    async fn concurrency_32_writes_the_same_documents_as_one() {
        async fn run_at(
            concurrency: u32,
        ) -> (
            Vec<(String, String, u32, u32, bool)>,
            Vec<(String, String, String, &'static str, Option<u32>)>,
        ) {
            let (launching, pack) = launching("captured_rows_count").await;
            let mut request = request(&launching, &pack, "run-1");
            request.concurrency = concurrency;
            request.trials_per_case = 8;
            let outcome = run(
                &launching.access,
                &request,
                &ScriptedExecutor::new().with_default(passed()),
                &CheckRegistry::builtin(),
                CancellationToken::new(),
                &options(),
            )
            .await
            .unwrap();
            assert_eq!(outcome.completed, 32);
            let trials = load_trials(&launching.access, OWNER, "run-1")
                .await
                .unwrap();
            let verdicts = load_verdicts(&launching.access, OWNER, "run-1")
                .await
                .unwrap();
            (trial_keys(&trials), verdict_keys(&verdicts))
        }

        let (serial_trials, serial_verdicts) = run_at(1).await;
        let (parallel_trials, parallel_verdicts) = run_at(32).await;
        assert_eq!(serial_trials, parallel_trials);
        assert_eq!(serial_verdicts, parallel_verdicts);
    }

    #[test]
    fn a_stage_captures_what_it_declares_and_otherwise_what_the_run_requested() {
        let case: EvalCase = serde_json::from_value(json!({
            "case_id": "k",
            "split": "train",
            "stages": [
                {
                    "stage_id": "declared",
                    "prompt": "p",
                    "deadline_secs": 60,
                    "capture": [
                        {
                            "kind": "documents",
                            "name": "items",
                            "collection": "MailboxItem",
                            "filter": {"requester_did": {"_eq": "$trial"}},
                            "fields": ["title", "status"]
                        },
                        {"kind": "file", "name": "notes", "glob": "**/*.txt"}
                    ]
                },
                {"stage_id": "bare", "prompt": "p"},
                {
                    "stage_id": "seeded",
                    "seed": {"collection": "Event", "document": {"kind": "signup"}},
                    "deadline_secs": 60
                }
            ]
        }))
        .unwrap();
        let fallback = vec![Capture::Documents {
            name: "rows".into(),
            collection: "Row".into(),
            filter: json!({}),
            fields: Vec::new(),
        }];

        let stages = stage_specs(&case, &fallback);

        assert_eq!(
            (stages[0].stage_id.as_str(), stages[0].deadline_secs),
            ("declared", Some(60))
        );
        assert_eq!(stages[1].deadline_secs, None);
        assert_eq!(
            stages[0].captures,
            vec![
                Capture::Documents {
                    name: "items".into(),
                    collection: "MailboxItem".into(),
                    filter: json!({"requester_did": {"_eq": "$trial"}}),
                    fields: vec!["title".into(), "status".into()],
                },
                Capture::File {
                    name: "notes".into(),
                    glob: "**/*.txt".into(),
                },
            ]
        );
        assert_eq!(stages[1].captures, fallback);
        assert_eq!(stages[0].seed, None);
        assert_eq!(
            stages[2].seed,
            Some(FixtureDocument {
                collection: "Event".into(),
                document: json!({"kind": "signup"}),
            })
        );
    }

    #[tokio::test]
    async fn a_cancelled_slot_is_planned_again_under_no_infrastructure_retries() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-abandoned");
        one_slot(&mut request);
        request.max_infra_retries = 0;
        let cancel = CancellationToken::new();
        let outcome = run(
            &launching.access,
            &request,
            &CancelOnFirstExecute {
                run: cancel.clone(),
            },
            &CheckRegistry::builtin(),
            cancel,
            &options(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.abandoned), (0, 1));

        let resumed = resume(
            &launching.access,
            OWNER,
            "run-abandoned",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(resumed.completed, 1, "the abandoned attempt spent no retry");
        let mut attempts: Vec<(u32, bool)> = load_trials(&launching.access, OWNER, "run-abandoned")
            .await
            .unwrap()
            .iter()
            .map(|trial| (trial.identity.attempt, trial.completion.is_some()))
            .collect();
        attempts.sort();
        assert_eq!(attempts, vec![(1, false), (2, true)]);
    }

    struct StageReporting {
        run_dir: PathBuf,
        seen: Mutex<Vec<Option<Progress>>>,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for StageReporting {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            spec.progress.stage_started("check");
            let during = read_progress(&self.run_dir);
            spec.progress.stage_ended("check");
            self.seen.lock().unwrap().push(during);
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    #[tokio::test]
    async fn progress_names_the_stage_an_in_flight_slot_is_in_and_forgets_it_at_the_end() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-progress");
        one_slot(&mut request);
        let run_dir = launching.runs_dir().join("run-progress");
        let executor = StageReporting {
            run_dir: run_dir.clone(),
            seen: Mutex::new(Vec::new()),
        };
        run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();

        let seen = executor.seen.lock().unwrap().clone();
        let during = seen[0]
            .as_ref()
            .expect("progress.json exists while the trial runs");
        let (trial_id, slot) = during.slots.iter().next().expect("one slot in flight");
        assert_eq!(
            trial_id,
            &trial_id_for("run-progress", "base", "case-a", 0, 1)
        );
        assert_eq!(
            (
                slot.cell_id.as_str(),
                slot.case_id.as_str(),
                slot.trial_index,
                slot.attempt,
                slot.stage_id.as_deref()
            ),
            ("base", "case-a", 0, 1, Some("check"))
        );
        let holder = during.holder.as_ref().expect("the loop holds the run");
        assert_eq!(holder.pid, std::process::id());
        let after = read_progress(&run_dir).expect("the file stays after the run");
        assert!(after.slots.is_empty(), "{after:?}");
        assert_eq!(after.holder, None, "the loop released the run: {after:?}");
        assert!(!running_elsewhere(&run_dir));
    }

    struct MarksThenWaits {
        runs_dir: PathBuf,
        run_id: String,
        interrupted: AtomicBool,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for MarksThenWaits {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
            request_cancel(&self.runs_dir, &self.run_id).unwrap();
            tokio::select! {
                _ = cancel.cancelled() => self.interrupted.store(true, Ordering::SeqCst),
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    struct DrainsSlowly {
        runs_dir: PathBuf,
        run_id: String,
        seen: Mutex<Vec<Option<String>>>,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for DrainsSlowly {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
            request_cancel(&self.runs_dir, &self.run_id).unwrap();
            tokio::time::timeout(Duration::from_secs(10), cancel.cancelled())
                .await
                .expect("the loop saw the marker");
            let holder = || {
                read_progress(&self.runs_dir.join(&self.run_id))
                    .and_then(|progress| progress.holder)
                    .map(|holder| holder.written_at)
            };
            let first = holder();
            tokio::time::sleep(Duration::from_millis(1_000)).await;
            let second = holder();
            self.seen.lock().unwrap().extend([first, second]);
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    #[tokio::test]
    async fn the_holder_stays_fresh_while_a_cancelled_pass_drains() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-drain");
        one_slot(&mut request);
        let executor = DrainsSlowly {
            runs_dir: launching.runs_dir(),
            run_id: "run-drain".into(),
            seen: Mutex::new(Vec::new()),
        };
        run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let seen = executor.seen.lock().unwrap().clone();
        let (Some(first), Some(second)) = (&seen[0], &seen[1]) else {
            panic!("the loop held the run while it drained: {seen:?}");
        };
        assert!(
            second > first,
            "the 250 ms heartbeat refreshed the holder during the drain: {seen:?}"
        );
        let after = read_progress(&launching.runs_dir().join("run-drain")).unwrap();
        assert_eq!(after.holder, None);
    }

    #[tokio::test]
    async fn a_marker_written_mid_trial_interrupts_the_trial_in_flight() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-mid");
        one_slot(&mut request);
        let executor = MarksThenWaits {
            runs_dir: launching.runs_dir(),
            run_id: "run-mid".into(),
            interrupted: AtomicBool::new(false),
        };
        let outcome = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert!(
            executor.interrupted.load(Ordering::SeqCst),
            "the trial saw its token cancelled, not its 30 s timeout"
        );
        assert_eq!((outcome.completed, outcome.abandoned), (0, 1));
        let trials = load_trials(&launching.access, OWNER, "run-mid")
            .await
            .unwrap();
        assert_eq!(trials.len(), 1);
        assert!(trials[0].completion.is_none(), "left open for a resume");
    }

    struct WatchesHeartbeat {
        run_dir: PathBuf,
        seen: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for WatchesHeartbeat {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            let written_at = || {
                read_progress(&self.run_dir)
                    .and_then(|progress| progress.slots.values().next().cloned())
                    .map(|slot| slot.written_at)
                    .unwrap_or_default()
            };
            let first = written_at();
            tokio::time::sleep(Duration::from_millis(600)).await;
            let second = written_at();
            self.seen.lock().unwrap().extend([first, second]);
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    struct ReportsLive {
        run_dir: PathBuf,
        seen: Mutex<Option<InFlight>>,
    }

    fn snapshot(requests: u64) -> LiveSnapshot {
        LiveSnapshot {
            stages: Default::default(),
            observed_at: "2026-09-29T00:00:00Z".into(),
            elapsed_secs: requests * 10,
            requests,
            model_turns: 4 * requests,
            input_tokens: Some(1_200 * requests),
            output_tokens: Some(300 * requests),
            reported_input_tokens: None,
            reported_output_tokens: None,
            session_contexts: None,
            tool_calls: 3,
            failed_tool_calls: 1,
            tools: std::collections::BTreeMap::from([(
                "config".to_owned(),
                ToolTally {
                    calls: 3,
                    failed: 1,
                },
            )]),
            documents: std::collections::BTreeMap::from([("Finding".to_owned(), requests)]),
            captures: std::collections::BTreeMap::from([("findings".to_owned(), requests)]),
            schemas: vec!["Finding".into()],
            last_tool: None,
        }
    }

    #[async_trait::async_trait]
    impl TrialExecutor for ReportsLive {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_node_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            assert!(spec.progress.attached(), "the loop watches every trial");
            spec.progress.live(snapshot(1));
            *self.seen.lock().unwrap() = read_progress(&self.run_dir)
                .and_then(|progress| progress.slots.values().next().cloned());
            spec.progress.live(snapshot(2));
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    #[tokio::test]
    async fn a_slots_live_snapshot_is_in_progress_and_its_last_is_kept_with_its_evidence() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-live");
        one_slot(&mut request);
        request.captures = vec![Capture::Documents {
            name: "findings".into(),
            collection: "Finding".into(),
            filter: json!({}),
            fields: Vec::new(),
        }];
        let run_dir = launching.runs_dir().join("run-live");
        let executor = ReportsLive {
            run_dir: run_dir.clone(),
            seen: Mutex::new(None),
        };
        run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();

        let goal = vec![GoalEntry {
            collection: "Finding".into(),
            capture: Some("findings".into()),
            min: 1,
            max: None,
        }];
        let in_flight = executor.seen.lock().unwrap().clone().expect("in flight");
        assert_eq!(in_flight.live, Some(snapshot(1)));
        assert_eq!(
            in_flight.goal, goal,
            "derived from the case's row-count check"
        );

        let trials = load_trials(&launching.access, OWNER, "run-live")
            .await
            .unwrap();
        let trial_id = &trials[0].identity.trial_id;
        let record = read_evidence_record(&run_dir.join("trials").join(trial_id))
            .expect("an evidence record");
        assert_eq!(record.live, Some(snapshot(2)), "the last snapshot reported");
        assert_eq!(record.goal, goal);
        assert!(record.ended_at.is_some());

        let view = read_run_view(&run_dir).expect("report.json");
        assert_eq!(view.pid, std::process::id());
        assert_eq!(view.trials.get(trial_id), Some(&record));
        let slot = &view.report.cells[0].slots[0];
        assert_eq!(slot.class, crate::eval::report::SlotClass::Pass);
        assert_eq!(
            slot.verdicts[0].detail.as_deref(),
            Some("findings observed 1 expected ≥1")
        );
        assert_eq!(
            view.report,
            crate::eval::report::load_report(
                &launching.access,
                OWNER,
                &launching.runs_dir(),
                "run-live"
            )
            .await
            .unwrap(),
            "the file is the report the node derives"
        );
    }

    struct BackoffBegan(Arc<tokio::sync::Notify>);

    impl<S> tracing_subscriber::Layer<S> for BackoffBegan
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == module_path!().trim_end_matches("::tests")
                && event.metadata().fields().field("backoff").is_some()
            {
                self.0.notify_one();
            }
        }
    }

    #[tokio::test]
    async fn a_marker_written_during_the_backoff_ends_the_wait() {
        use tracing_subscriber::layer::SubscriberExt;

        let began = Arc::new(tokio::sync::Notify::new());
        let subscriber = tracing::Dispatch::new(
            tracing_subscriber::Registry::default().with(BackoffBegan(began.clone())),
        );
        let _subscriber = tracing::dispatcher::set_default(&subscriber);

        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-backoff");
        one_slot(&mut request);
        let slow = RunOptions {
            trial_budget: None,
            poll_backoff_base: Duration::from_secs(30),
            poll_backoff_cap: Duration::from_secs(30),
        };
        let runs_dir = launching.runs_dir();
        let run_dir = runs_dir.join("run-backoff");
        let held = || {
            read_progress(&run_dir)
                .and_then(|progress| progress.holder)
                .map(|holder| holder.written_at)
        };
        let operator = async {
            tokio::time::timeout(Duration::from_secs(20), began.notified())
                .await
                .expect("the loop began its backoff after the not-evidence attempt");
            let first = held().expect("the loop holds the run during its backoff");
            assert!(running_elsewhere(&run_dir));
            tokio::time::sleep(Duration::from_millis(2_500)).await;
            let second = held().expect("still held");
            assert!(
                second > first,
                "refreshed during the backoff: {first} {second}"
            );
            request_cancel(&runs_dir, "run-backoff").unwrap();
            std::time::Instant::now()
        };
        let executor =
            ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:trial"));
        let registry = CheckRegistry::builtin();
        let (outcome, marked) = tokio::join!(
            run(
                &launching.access,
                &request,
                &executor,
                &registry,
                CancellationToken::new(),
                &slow,
            ),
            operator,
        );
        let outcome = outcome.unwrap();
        assert!(
            marked.elapsed() < Duration::from_secs(5),
            "the 30 s backoff was cut short at the next tick: {:?}",
            marked.elapsed()
        );
        assert!(outcome.cancelled);
        assert_eq!((outcome.completed, outcome.abandoned), (1, 0));
        assert_eq!(
            load_trials(&launching.access, OWNER, "run-backoff")
                .await
                .unwrap()
                .len(),
            1,
            "the retry the backoff waited for never launched"
        );
        assert_eq!(held(), None, "a cancelled loop releases the run");
        assert!(!running_elsewhere(&run_dir));
    }

    #[tokio::test]
    async fn the_loop_refreshes_in_flight_entries_while_a_trial_runs() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-beat");
        one_slot(&mut request);
        let executor = WatchesHeartbeat {
            run_dir: launching.runs_dir().join("run-beat"),
            seen: Mutex::new(Vec::new()),
        };
        run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let seen = executor.seen.lock().unwrap().clone();
        assert!(!seen[0].is_empty(), "{seen:?}");
        assert!(
            seen[1] > seen[0],
            "written_at advanced while the trial ran: {seen:?}"
        );
        assert_eq!(marker_poll(&options()), Duration::from_millis(1));
        assert_eq!(marker_poll(&RunOptions::default()), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_run_is_finished_when_it_owes_nothing_and_no_live_process_holds_a_slot() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-fin");
        one_slot(&mut request);
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let record = load_run(&launching.access, OWNER, "run-fin")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots_owed(&record, &[]), 1);
        assert!(!run_finished(&record, &[], &frozen.run_dir));

        run(
            &launching.access,
            &request,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let trials = load_trials(&launching.access, OWNER, "run-fin")
            .await
            .unwrap();
        assert_eq!(slots_owed(&record, &trials), 0);
        assert!(run_finished(&record, &trials, &frozen.run_dir));

        let writer = ProgressWriter::new(&frozen.run_dir);
        writer.slot_started(
            "held",
            InFlight {
                cell_id: "base".into(),
                case_id: "case-a".into(),
                trial_index: 0,
                attempt: 2,
                stage_id: None,
                started_at: String::new(),
                pid: 0,
                written_at: String::new(),
                live: None,
                goal: Vec::new(),
            },
        );
        assert!(running_elsewhere(&frozen.run_dir));
        assert!(!run_finished(&record, &trials, &frozen.run_dir));

        let rewrite = |change: &dyn Fn(&mut InFlight)| {
            let mut progress = read_progress(&frozen.run_dir).unwrap();
            change(progress.slots.get_mut("held").unwrap());
            std::fs::write(
                frozen.run_dir.join(PROGRESS_FILE),
                serde_json::to_vec(&progress).unwrap(),
            )
            .unwrap();
        };
        rewrite(&|slot| slot.written_at = "2026-01-01T00:00:00.000Z".into());
        assert!(!running_elsewhere(&frozen.run_dir));

        rewrite(&|slot| {
            slot.written_at =
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            slot.pid = u32::MAX;
        });
        assert!(!running_elsewhere(&frozen.run_dir));
        assert!(run_finished(&record, &trials, &frozen.run_dir));

        let idle = ProgressWriter::new(&frozen.run_dir);
        let held = idle.hold();
        assert!(running_elsewhere(&frozen.run_dir));
        assert!(!run_finished(&record, &trials, &frozen.run_dir));
        let mut progress = read_progress(&frozen.run_dir).unwrap();
        progress.holder.as_mut().unwrap().written_at = "2026-01-01T00:00:00.000Z".into();
        std::fs::write(
            frozen.run_dir.join(PROGRESS_FILE),
            serde_json::to_vec(&progress).unwrap(),
        )
        .unwrap();
        assert!(!running_elsewhere(&frozen.run_dir));
        drop(held);
        assert!(!running_elsewhere(&frozen.run_dir));

        assert_eq!(STALE_WINDOW, marker_poll(&RunOptions::default()) * 3);
        assert!(
            [0, 1, 250, 999, 1_000, 5_000, u64::MAX].iter().all(|ms| {
                let options = RunOptions {
                    trial_budget: None,
                    poll_backoff_base: Duration::from_millis(*ms),
                    poll_backoff_cap: Duration::from_secs(60),
                };
                marker_poll(&options) * 3 <= STALE_WINDOW
            }),
            "one window covers a runner started with any options"
        );
    }
}
