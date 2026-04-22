//! Manual-fire `TriggerSource`.
//!
//! Dispatches operator-initiated manual runs through the trigger engine.
//! Pushed into by `ManualTriggerHandle::run_task_now` (added in Task 2).
//! Unlike `ScheduleSource` and `EventSource`, `ManualSource` has no
//! DB-watching loop — it sits on an mpsc and yields whatever intents
//! show up.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{FireIntent, TriggerSource};

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
/// receiver that resolves when the engine's on_result callback fires
/// (full run_task_now API lands in Task 2).
#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct ManualTriggerHandle {
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
