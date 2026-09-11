//! Schedule-backed `TriggerSource`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::document_config::{
    load_trigger_next_run_at, update_trigger_runtime_fields, TriggerRuntimeUpdate,
};
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::trigger_engine::{FireIntent, FireResult, TriggerKind, TriggerSource};

pub(crate) struct ScheduleSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    tick_every: Duration,
    cancel: CancellationToken,
}

impl ScheduleSource {
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
                tokio::select! {
                    _ = tokio::time::sleep(self.tick_every) => {}
                    _ = self.cancel.cancelled() => return None,
                }

                let snapshot = self.snapshot_rx.borrow().clone();
                let now = Utc::now();

                for (trigger_id, resolved) in snapshot.active_schedules() {
                    let Some(behavior) = snapshot.behavior(&resolved.task.behavior_id) else {
                        continue;
                    };
                    let agent_did = behavior.agent_did();
                    let next_run_at = match load_trigger_next_run_at(
                        &self.node, agent_did, trigger_id,
                    )
                    .await
                    {
                        Ok(Some(s)) => s,
                        Ok(None) => {
                            let seeded_dt = match crate::runtime_snapshot::seed_schedule_next_run_at(
                                &resolved.cadence,
                                now,
                            ) {
                                Ok(next) => next,
                                Err(e) => {
                                    tracing::warn!(
                                        trigger_id = %trigger_id,
                                        error = %e,
                                        "failed to compute initial Trigger.next_run_at; skipping this tick"
                                    );
                                    continue;
                                }
                            };
                            let seeded = seeded_dt.to_rfc3339_opts(SecondsFormat::Secs, true);
                            if let Err(e) = update_trigger_runtime_fields(
                                &self.node,
                                agent_did,
                                trigger_id,
                                TriggerRuntimeUpdate {
                                    last_fired_source_doc_id: None,
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
                                    trigger_id = %trigger_id,
                                    error = %e,
                                    "failed to seed Trigger.next_run_at on first-seen; \
                                     will retry next tick"
                                );
                                continue;
                            }
                            seeded
                        }
                        Err(e) => {
                            tracing::warn!(
                                trigger_id = %trigger_id,
                                error = %e,
                                "failed to load Trigger.next_run_at; skipping this tick"
                            );
                            continue;
                        }
                    };

                    let parsed = match DateTime::parse_from_rfc3339(&next_run_at) {
                        Ok(dt) => dt.with_timezone(&Utc),
                        Err(e) => {
                            tracing::warn!(
                                trigger_id = %trigger_id,
                                next_run_at = %next_run_at,
                                error = %e,
                                "Trigger.next_run_at is not valid RFC3339; skipping"
                            );
                            continue;
                        }
                    };

                    if parsed > now {
                        continue;
                    }

                    let fired_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
                    let event_vars = serde_json::json!({
                        "fired_at": fired_at,
                        "trigger_id": trigger_id,
                        "trigger_kind": "schedule",
                    });

                    let advanced_next_run_at =
                        match crate::runtime_snapshot::advance_schedule_next_run_at(
                            &resolved.cadence,
                            parsed,
                            now,
                        ) {
                            Ok(next) => next,
                            Err(e) => {
                                tracing::warn!(
                                    trigger_id = %trigger_id,
                                    error = %e,
                                    "failed to compute advanced Trigger.next_run_at; skipping this tick"
                                );
                                continue;
                            }
                        };
                    let advanced_next_run_at_str =
                        advanced_next_run_at.to_rfc3339_opts(SecondsFormat::Secs, true);
                    let last_attempt_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);

                    let node_for_callback = self.node.clone();
                    let trigger_id_for_callback = trigger_id.clone();
                    let owner_for_callback = agent_did.to_owned();

                    return Some(FireIntent {
                        trigger_id: Some(trigger_id.clone()),
                        trigger_kind: TriggerKind::Schedule,
                        task: resolved.task.clone(),
                        concurrency: resolved.concurrency,
                        event_vars,
                        doc_vars: None,
                        correlation: None,
                        group_vars: None,
                        trigger_context: None,
                        args_vars: None,
                        durable_fire_key: crate::trigger_engine::durable_fire_key(
                            "schedule",
                            &[&trigger_id, &next_run_at],
                        ),
                        pre_materialized_request_id: None,
                        on_result: Box::new(move |result| {
                            let updates = match &result {
                                FireResult::Fired { request_id } => {
                                    tracing::debug!(
                                        trigger_id = %trigger_id_for_callback,
                                        request_id = %request_id,
                                        "schedule fire materialized request"
                                    );
                                    TriggerRuntimeUpdate {
                                        last_fired_source_doc_id: None,
                                        next_run_at: Some(advanced_next_run_at_str.clone()),
                                        last_attempt_at: Some(last_attempt_at.clone()),
                                        last_status: Some("fired".to_string()),
                                        last_error: None,
                                        fire_count_delta: Some(1),
                                    }
                                }
                                FireResult::Skipped { .. } => TriggerRuntimeUpdate {
                                    last_fired_source_doc_id: None,
                                    next_run_at: Some(advanced_next_run_at_str.clone()),
                                    last_attempt_at: Some(last_attempt_at.clone()),
                                    last_status: Some("skipped".to_string()),
                                    last_error: None,
                                    fire_count_delta: None,
                                },
                                FireResult::Errored { error } => TriggerRuntimeUpdate {
                                    last_fired_source_doc_id: None,
                                    next_run_at: None,
                                    last_attempt_at: Some(last_attempt_at.clone()),
                                    last_status: Some("error".to_string()),
                                    last_error: Some(error.clone()),
                                    fire_count_delta: None,
                                },
                            };
                            tokio::spawn(async move {
                                if let Err(e) = update_trigger_runtime_fields(
                                    &node_for_callback,
                                    &owner_for_callback,
                                    &trigger_id_for_callback,
                                    updates,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        trigger_id = %trigger_id_for_callback,
                                        error = %e,
                                        "failed to write Trigger runtime fields after fire",
                                    );
                                }
                            });
                        }),
                    });
                }
            }
        })
    }
}
