use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::eval::runner::{freeze, is_fresh, plan, read_progress, FreezeRefused, RunOptions};
use crate::eval::{RunRecord, TrialRecord};

pub const CANCEL_MARKER: &str = "cancel";

pub fn run_dir(runs_dir: &Path, run_id: &str) -> Result<PathBuf> {
    freeze::validate::directory_name("run_id", run_id)?;
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

pub(super) fn cancel_requested(run_dir: &Path, cancel: &CancellationToken) -> bool {
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

pub(super) fn clear_cancel(run_dir: &Path) -> Result<()> {
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

pub(super) fn refuse_if_held(run_dir: &Path) -> Result<()> {
    if running_elsewhere(run_dir) {
        return Err(anyhow::Error::from(FreezeRefused(format!(
            "run {} is still running in another process",
            run_dir.display()
        ))));
    }
    Ok(())
}

pub(super) fn marker_poll(options: &RunOptions) -> Duration {
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
