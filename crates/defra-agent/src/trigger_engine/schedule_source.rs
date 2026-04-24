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

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::document_config::{
    load_schedule_next_run_at, update_schedule_runtime_fields, ScheduleRuntimeUpdate,
};
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::trigger_engine::{FireIntent, FireResult, TriggerKind, TriggerSource};

/// `TriggerSource` that drives the schedule clock.
///
/// Reads enabled schedules from the active runtime snapshot, ticks at
/// `tick_every` (default: 1s), and yields a `FireIntent` whenever a schedule's
/// `next_run_at` has passed. Honors `cancel` for graceful shutdown.
pub(crate) struct ScheduleSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    tick_every: Duration,
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
            // Loop until either (a) some schedule becomes due and we return
            // `Some(intent)`, or (b) the cancellation token fires and we
            // return `None`. The contract of this method: `None` means
            // "source is permanently done, drop it" — an idle tick (no due
            // schedule) must NOT exit. The engine's outer loop treats `None`
            // as source exhaustion and breaks, so a premature `None` here
            // used to kill the schedule driver after the first idle tick.
            loop {
                // Step 1: sleep one tick before scanning so the source doesn't
                // spam-query immediately on construction. Honor the
                // cancellation token at this tick boundary so a caller that
                // drops or cancels the token gets a prompt `None` back
                // instead of having to wait for the full tick to elapse.
                tokio::select! {
                    _ = tokio::time::sleep(self.tick_every) => {}
                    _ = self.cancel.cancelled() => return None,
                }

                // Step 2: snapshot-read the active schedules. For each
                // active schedule, if `next_run_at` is null the schedule was
                // just created — apply-path writers (CLI, desktop) only
                // touch apply-owned fields, so the runtime owns the first
                // write of this runtime-owned field. Seed with `now` so the
                // schedule fires on this same tick rather than sitting inert
                // forever.
                let snapshot = self.snapshot_rx.borrow().clone();
                let now = Utc::now();

                for (schedule_id, resolved) in snapshot.active_schedules() {
                    let next_run_at = match load_schedule_next_run_at(&self.node, schedule_id).await
                    {
                        Ok(Some(s)) => s,
                        Ok(None) => {
                            // Seed `next_run_at = now` on first-seen. The
                            // apply/reconcile path never writes runtime-owned
                            // fields, so this value starts null until the
                            // engine initializes it here. Treat as due on
                            // this very tick so first-fire latency stays
                            // below one full interval.
                            let seeded = now.to_rfc3339_opts(SecondsFormat::Secs, true);
                            if let Err(e) = update_schedule_runtime_fields(
                                &self.node,
                                schedule_id,
                                ScheduleRuntimeUpdate {
                                    next_run_at: Some(seeded.clone()),
                                    last_attempt_at: None,
                                    last_status: None,
                                    last_error: None,
                                    fire_count_delta: None,
                                },
                            )
                            .await
                            {
                                tracing::warn!(
                                    schedule_id = %schedule_id,
                                    error = %e,
                                    "failed to seed Schedule.next_run_at on first-seen; \
                                     will retry next tick"
                                );
                                continue;
                            }
                            seeded
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
                    // Normalize RFC3339 output to `Z`-suffix with second
                    // precision so the strings we write back round-trip
                    // cleanly through DefraDB's DateTime scalar. The default
                    // `to_rfc3339()` emits microsecond precision with a
                    // `+00:00` offset, which DefraDB's schema validator
                    // round-trips inconsistently across subsequent updates.
                    // This matches the `normalize_optional_rfc3339` helper
                    // already used elsewhere in the codebase.
                    let fired_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
                    let event_vars = serde_json::json!({
                        "fired_at": fired_at,
                        "trigger_id": schedule_id,
                        "trigger_kind": "schedule",
                    });

                    // Precompute the advanced next_run_at using the DB-loaded
                    // `parsed` (not `now + interval`) so schedules that got
                    // behind still advance on a single-interval cadence. The
                    // DB write itself happens in the `on_result` callback
                    // below, off the engine's dispatch path.
                    let advanced_next_run_at =
                        parsed + ChronoDuration::seconds(resolved.interval_secs);
                    let advanced_next_run_at_str =
                        advanced_next_run_at.to_rfc3339_opts(SecondsFormat::Secs, true);
                    let last_attempt_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);

                    // Values captured for the result-writeback closure. The
                    // callback is synchronous (`FnOnce(FireResult)`), so it
                    // spawns a background task that performs the DefraDB
                    // mutation; the engine's dispatch loop continues without
                    // waiting on bookkeeping I/O.
                    let node_for_callback = self.node.clone();
                    let schedule_id_for_callback = schedule_id.clone();

                    return Some(FireIntent {
                        trigger_id: Some(schedule_id.clone()),
                        trigger_kind: TriggerKind::Schedule,
                        task: resolved.task.clone(),
                        concurrency: resolved.concurrency,
                        event_vars,
                        doc_vars: None,
                        args_vars: None,
                        on_result: Box::new(move |result| {
                            let updates = match &result {
                                FireResult::Fired { request_id } => {
                                    tracing::debug!(
                                        schedule_id = %schedule_id_for_callback,
                                        request_id = %request_id,
                                        "schedule fire materialized request"
                                    );
                                    ScheduleRuntimeUpdate {
                                        next_run_at: Some(advanced_next_run_at_str.clone()),
                                        last_attempt_at: Some(last_attempt_at.clone()),
                                        last_status: Some("fired".to_string()),
                                        last_error: None,
                                        fire_count_delta: Some(1),
                                    }
                                }
                                FireResult::Skipped { .. } => ScheduleRuntimeUpdate {
                                    // Skipped still advances `next_run_at` so
                                    // a serial-gated fire doesn't hammer the
                                    // clock every tick — the intent was that
                                    // this tick "happened", just without
                                    // materializing.
                                    next_run_at: Some(advanced_next_run_at_str.clone()),
                                    last_attempt_at: Some(last_attempt_at.clone()),
                                    last_status: Some("skipped".to_string()),
                                    last_error: None,
                                    fire_count_delta: None,
                                },
                                FireResult::Errored { error } => ScheduleRuntimeUpdate {
                                    // Don't advance next_run_at on error so
                                    // the next tick retries this fire; only
                                    // record last_* bookkeeping.
                                    next_run_at: None,
                                    last_attempt_at: Some(last_attempt_at.clone()),
                                    last_status: Some("error".to_string()),
                                    last_error: Some(error.clone()),
                                    fire_count_delta: None,
                                },
                            };
                            tokio::spawn(async move {
                                if let Err(e) = update_schedule_runtime_fields(
                                    &node_for_callback,
                                    &schedule_id_for_callback,
                                    updates,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        schedule_id = %schedule_id_for_callback,
                                        error = %e,
                                        "failed to write Schedule runtime fields after fire",
                                    );
                                }
                            });
                        }),
                    });
                }

                // No schedule was due on this tick. Continue the outer loop
                // (which will re-sleep or honor cancel) instead of returning
                // `None` — `None` would tell the engine this source is dead
                // and break it out of the dispatch loop forever.
            }
        })
    }
}
