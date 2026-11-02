use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::eval::checks::CheckRegistry;
use crate::eval::runner::liveness::cancel_requested;
use crate::eval::runner::progress::ProgressWriter;
use crate::eval::runner::projection::{completion, verdict_draft};
use crate::eval::runner::spec::trial_spec;
use crate::eval::runner::{
    case_goal, completion_is_not_evidence, grade, run_view, view, EvidenceRecord, FrozenRun,
    InFlight, PlannedTrial, Recorder, StageProgress, TrialBudget, TrialExecutor,
};
use crate::eval::TrialIdentity;

pub(super) enum Slot {
    Skipped,
    Abandoned,
    Completed { attempt: u32, not_evidence: bool },
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_trial(
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
pub(super) struct ViewWriter(tokio::sync::Mutex<()>);

impl ViewWriter {
    pub(super) async fn refresh(&self, frozen: &FrozenRun, recorder: &dyn Recorder) {
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
