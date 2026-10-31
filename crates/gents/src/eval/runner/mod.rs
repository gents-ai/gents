pub mod embedded;
pub mod executor;
mod files;
pub mod freeze;
pub mod goal;
pub mod grade;
mod liveness;
pub mod plan;
pub mod progress;
mod projection;
pub mod record;
pub mod review;
pub mod scripted;
mod spec;
mod trial;
pub mod view;

pub use executor::{
    Capture, CaptureResult, FileRef, FixtureDocument, FixtureFile, InferenceBinding, Isolation,
    StageEvidence, StageSpec, TrialEvidence, TrialExecutor, TrialFixtures, TrialLocator, TrialSpec,
};
pub use freeze::snapshot::{read_frozen_definition, DEFINITION_FILE};
pub use freeze::{
    freeze, freeze_refused, CellRequest, CellSource, FreezeRefused, FrozenCell, FrozenRun,
    RunRequest,
};
pub use goal::{case_goal, GoalEntry, SCHEMAS_GOAL};
pub use grade::{grade, VerdictRow};
pub use liveness::{
    request_cancel, run_dir, run_finished, running_elsewhere, slots_owed, CANCEL_MARKER,
    STALE_WINDOW,
};
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

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::eval::checks::CheckRegistry;
use crate::eval::runner::freeze::thaw;
use crate::eval::runner::liveness::{cancel_requested, clear_cancel, marker_poll, refuse_if_held};
use crate::eval::runner::progress::ProgressWriter;
use crate::eval::runner::trial::{execute_trial, Slot, ViewWriter};
use crate::eval::TrialRecord;
use crate::store::config_client::ConfigAccess;

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
mod tests;
