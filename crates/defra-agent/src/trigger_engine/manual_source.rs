//! Manual-fire `TriggerSource`.
//!
//! Dispatches operator-initiated manual runs through the trigger engine.
//! Pushed into by `ManualTriggerHandle::run_task_now` (added in Task 2).
//! Unlike `ScheduleSource` and `EventSource`, `ManualSource` has no
//! DB-watching loop — it sits on an mpsc and yields whatever intents
//! show up.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{FireIntent, FireResult, TriggerKind, TriggerSource};
use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ConcurrencyMode};

/// Channel bound: each pending manual fire sits here awaiting dispatch.
/// 32 is generous — callers should get immediate handoff in practice.
const MANUAL_CHANNEL_CAPACITY: usize = 32;

/// Dispatches operator-initiated manual runs through the trigger engine.
///
/// Pushed into by `ManualTriggerHandle::run_task_now` (added in Task 2).
/// Unlike `ScheduleSource` and `EventSource`, `ManualSource` has no
/// DB-watching loop — it sits on an mpsc and yields whatever intents
/// show up.
#[allow(dead_code)]
pub(crate) struct ManualSource {
    rx: mpsc::Receiver<FireIntent>,
    cancel: CancellationToken,
}

/// Clonable handle distributed to in-process callers (desktop, API
/// consumers). Holds a `Sender`; pushing an intent yields a oneshot
/// receiver that resolves when the engine's `on_result` callback fires.
#[derive(Clone)]
pub(crate) struct ManualTriggerHandle {
    // `#[allow(dead_code)]`: the field is exercised by `run_task_now`
    // (and by tests on it), but the handle itself is not yet constructed
    // outside tests — Task 4 wires `ManualSource::new` into startup.
    #[allow(dead_code)]
    tx: mpsc::Sender<FireIntent>,
}

impl ManualSource {
    #[allow(dead_code)]
    pub(crate) fn new(cancel: CancellationToken) -> (Self, ManualTriggerHandle) {
        let (tx, rx) = mpsc::channel(MANUAL_CHANNEL_CAPACITY);
        (
            Self { rx, cancel },
            ManualTriggerHandle { tx },
        )
    }
}

impl ManualTriggerHandle {
    /// Enqueue a manual run for the given `task_id` with operator-supplied
    /// `args`.
    ///
    /// Returns a oneshot receiver that resolves to the engine's
    /// `FireResult` once dispatch completes. Caller can `.await` for the
    /// request id or error.
    ///
    /// Errors if `task_id` is not present in the active snapshot (task
    /// missing, disabled, or its behavior unavailable) or if the engine's
    /// manual channel has shut down.
    #[allow(dead_code)]
    pub(crate) async fn run_task_now(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
        task_id: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<oneshot::Receiver<FireResult>> {
        let resolved_task = snapshot
            .active_tasks()
            .get(task_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "task {task_id} is not in the active snapshot (check the task exists, is enabled, and its behavior is available)"
                )
            })?;

        let (result_tx, result_rx) = oneshot::channel();
        let now =
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let intent = FireIntent {
            trigger_id: None,
            trigger_kind: TriggerKind::Manual,
            task: resolved_task,
            // Manual runs never queue behind a prior run. The engine's
            // concurrency gate is bypassed for `Parallel`.
            concurrency: ConcurrencyMode::Parallel,
            event_vars: serde_json::json!({
                "fired_at": now,
                "trigger_id": serde_json::Value::Null,
                "trigger_kind": "manual",
            }),
            doc_vars: None,
            args_vars: Some(args),
            on_result: Box::new(move |result| {
                // Caller may have dropped the receiver — that's fine.
                let _ = result_tx.send(result);
            }),
        };

        self.tx.send(intent).await.map_err(|_| {
            anyhow::anyhow!("manual trigger channel is closed; engine has shut down")
        })?;
        Ok(result_rx)
    }
}

impl TriggerSource for ManualSource {
    fn next_fire(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            tokio::select! {
                _ = self.cancel.cancelled() => None,
                intent = self.rx.recv() => intent,  // None if all senders dropped
            }
        })
    }
}
