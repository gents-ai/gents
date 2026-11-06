use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::documents::{
    load_trigger_next_run_at_batch, update_trigger_runtime_fields, TriggerRuntimeUpdate,
    TRIGGER_CURSOR_BATCH_SIZE,
};
use crate::runtime::snapshot::ActiveRuntimeSnapshot;
use crate::store::config_client::{ConfigAccess, ConfigClientError};
use crate::triggers::engine::{FireIntent, FireResult, TriggerKind, TriggerSource};

async fn schedule_visibility(
    node: &Arc<EmbeddedNode>,
    runtime_did: &str,
    doc_ids: &[String],
) -> Result<Vec<bool>, ConfigClientError> {
    if node.node_identity_did() != Some(runtime_did) {
        return Err(ConfigClientError::TransactionActorMismatch);
    }
    if doc_ids.iter().any(String::is_empty) {
        return Err(ConfigClientError::InvalidOwnerObservation);
    }
    let actor =
        identity::Did::new(runtime_did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
    ConfigAccess::local_as(node.clone(), actor)
        .transact_readonly("schedule.cached_visibility", |txn| {
            let doc_ids = doc_ids.to_vec();
            Box::pin(async move {
                let observations = txn.document_owners("Schedule", &doc_ids).await?;
                if observations.len() != doc_ids.len()
                    || observations
                        .iter()
                        .zip(&doc_ids)
                        .any(|(row, id)| row.doc_id != *id)
                {
                    return Err(ConfigClientError::InvalidOwnerObservation);
                }
                Ok(observations.into_iter().map(|row| row.visible).collect())
            })
        })
        .await
}

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

    #[cfg(test)]
    pub(crate) fn with_tick_every(mut self, tick_every: Duration) -> Self {
        self.tick_every = tick_every;
        self
    }
}

impl TriggerSource for ScheduleSource {
    fn next_fire(&mut self) -> Pin<Box<dyn Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            // `None` means "source permanently done, drop it"; an idle tick
            // must not exit, or the engine's outer loop kills the schedule
            // driver.
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(self.tick_every) => {}
                    _ = self.cancel.cancelled() => return None,
                }

                let snapshot = self.snapshot_rx.borrow().clone();
                let now = Utc::now();

                let schedules = snapshot
                    .active_schedules()
                    .iter()
                    .filter_map(|(trigger_id, resolved)| {
                        snapshot
                            .agent(&resolved.task.agent_did)
                            .map(|behavior| (trigger_id, resolved, behavior.node_did()))
                    })
                    .collect::<Vec<_>>();
                for schedule_chunk in schedules.chunks(TRIGGER_CURSOR_BATCH_SIZE) {
                    let mut groups: BTreeMap<&str, Vec<(usize, String)>> = BTreeMap::new();
                    for (index, &(_, resolved, node_did)) in schedule_chunk.iter().enumerate() {
                        groups
                            .entry(node_did)
                            .or_default()
                            .push((index, resolved.schedule_doc_id.clone()));
                    }
                    let mut visible = vec![false; schedule_chunk.len()];
                    for (node_did, group) in groups {
                        let ids = group.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>();
                        match schedule_visibility(&self.node, node_did, &ids).await {
                            Ok(allowed) => {
                                for ((index, _), allowed) in group.into_iter().zip(allowed) {
                                    visible[index] = allowed;
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "failed to observe cached Schedule visibility")
                            }
                        }
                    }
                    let schedule_chunk = schedule_chunk
                        .iter()
                        .copied()
                        .zip(visible)
                        .filter_map(|(schedule, visible)| visible.then_some(schedule))
                        .collect::<Vec<_>>();
                    let cursor_scopes = schedule_chunk
                        .iter()
                        .map(|&(trigger_id, _, node_did)| {
                            (node_did.to_owned(), trigger_id.to_string())
                        })
                        .collect::<Vec<_>>();
                    let cursors = load_trigger_next_run_at_batch(&self.node, &cursor_scopes).await;

                    for ((trigger_id, resolved, node_did), next_run_at_result) in
                        schedule_chunk.iter().copied().zip(cursors)
                    {
                        let next_run_at = match next_run_at_result {
                            Ok(Some(s)) => s,
                            Ok(None) => {
                                let seeded_dt =
                                    match crate::runtime::snapshot::seed_schedule_next_run_at(
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
                                    node_did,
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
                            match crate::runtime::snapshot::advance_schedule_next_run_at(
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
                        let owner_for_callback = node_did.to_owned();

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
                            durable_fire_key: crate::triggers::engine::durable_fire_key(
                                "schedule",
                                &[trigger_id, &next_run_at],
                            ),
                            pre_materialized_request_id: None,
                            event_source_generation: None,
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
                                    FireResult::Duplicate { .. } => TriggerRuntimeUpdate {
                                        last_fired_source_doc_id: None,
                                        next_run_at: Some(advanced_next_run_at_str.clone()),
                                        last_attempt_at: Some(last_attempt_at.clone()),
                                        last_status: Some("duplicate".to_string()),
                                        last_error: None,
                                        fire_count_delta: None,
                                    },
                                    FireResult::Skipped { .. } => TriggerRuntimeUpdate {
                                        last_fired_source_doc_id: None,
                                        next_run_at: Some(advanced_next_run_at_str.clone()),
                                        last_attempt_at: Some(last_attempt_at.clone()),
                                        last_status: Some("skipped".to_string()),
                                        last_error: None,
                                        fire_count_delta: None,
                                    },
                                    FireResult::Errored { error }
                                    | FireResult::Rejected { error } => TriggerRuntimeUpdate {
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
            }
        })
    }
}

#[cfg(test)]
mod tests;
