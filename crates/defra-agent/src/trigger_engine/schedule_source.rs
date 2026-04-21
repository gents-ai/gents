//! Schedule-backed `TriggerSource`.
//!
//! Polls the active runtime snapshot on a fixed tick for `Schedule` triggers
//! whose `next_run_at` has elapsed and emits a `FireIntent` for the first due
//! schedule. Subsequent due schedules are left for the next tick (one intent
//! per tick keeps the source cooperative with the engine's fairness loop).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::document_config::load_schedule_next_run_at;
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::trigger_engine::{FireIntent, TriggerKind, TriggerSource};

/// `TriggerSource` that drives the schedule clock.
///
/// Reads enabled schedules from the active runtime snapshot, ticks at
/// `tick_every` (default: 1s), and yields a `FireIntent` whenever a schedule's
/// `next_run_at` has passed. Honors `cancel` for graceful shutdown.
pub(crate) struct ScheduleSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    tick_every: Duration,
    #[allow(dead_code)]
    cancel: CancellationToken,
}

impl ScheduleSource {
    /// Build a schedule source with the default 1s tick cadence.
    pub(crate) fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            tick_every: Duration::from_secs(1),
            cancel,
        }
    }

    /// Override the tick cadence. Primarily used by tests to tighten the loop
    /// from the 1s default so `next_fire` resolves quickly.
    #[cfg(test)]
    pub(crate) fn with_tick_every(mut self, tick_every: Duration) -> Self {
        self.tick_every = tick_every;
        self
    }
}

impl TriggerSource for ScheduleSource {
    fn next_fire(&mut self) -> Pin<Box<dyn Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            // Step 1: sleep one tick before scanning so the source doesn't
            // spam-query immediately on construction. Task 37 will add cancel
            // integration here; for now we just sleep unconditionally.
            tokio::time::sleep(self.tick_every).await;

            // Step 2: snapshot-read the active schedules and scan for the
            // first one whose DB-resident `next_run_at` has elapsed.
            let snapshot = self.snapshot_rx.borrow().clone();
            let now = Utc::now();

            for (schedule_id, resolved) in snapshot.active_schedules() {
                let next_run_at = match load_schedule_next_run_at(&self.node, schedule_id).await {
                    Ok(Some(s)) => s,
                    Ok(None) => {
                        // No persisted next_run_at: treat as not-yet-scheduled
                        // and skip. The apply/reconcile path will seed a
                        // next_run_at when the schedule is first created.
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(
                            schedule_id = %schedule_id,
                            error = %e,
                            "failed to load Schedule.next_run_at; skipping this tick"
                        );
                        continue;
                    }
                };

                let parsed = match DateTime::parse_from_rfc3339(&next_run_at) {
                    Ok(dt) => dt.with_timezone(&Utc),
                    Err(e) => {
                        tracing::warn!(
                            schedule_id = %schedule_id,
                            next_run_at = %next_run_at,
                            error = %e,
                            "Schedule.next_run_at is not valid RFC3339; skipping"
                        );
                        continue;
                    }
                };

                if parsed > now {
                    continue;
                }

                // Due. Build and return a FireIntent for this schedule.
                let fired_at = now.to_rfc3339();
                let event_vars = serde_json::json!({
                    "fired_at": fired_at,
                    "trigger_id": schedule_id,
                    "trigger_kind": "schedule",
                });

                return Some(FireIntent {
                    trigger_id: Some(schedule_id.clone()),
                    trigger_kind: TriggerKind::Schedule,
                    task: resolved.task.clone(),
                    concurrency: resolved.concurrency,
                    event_vars,
                    doc_vars: None,
                    args_vars: None,
                    // Task 36 fills in the runtime-field writeback (last_*,
                    // fire_count, next_run_at advance); for Task 35 the
                    // callback is a no-op placeholder.
                    on_result: Box::new(|_result| { /* Task 36 */ }),
                });
            }

            None
        })
    }
}
