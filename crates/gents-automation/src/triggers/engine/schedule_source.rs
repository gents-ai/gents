use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use rapidhash::RapidHashMap;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::triggers::engine::{FireIntent, FireResult, TriggerSource};
use crate::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use gents_documents::task_delivery::TriggerKind;
use gents_documents::{
    load_trigger_next_run_at_batch, update_trigger_runtime_fields, TriggerRuntimeUpdate,
    TRIGGER_CURSOR_BATCH_SIZE,
};
use gents_store::config_client::{ConfigAccess, ConfigClientError};

const SCHEDULE_FALLBACK_POLL: Duration = Duration::from_secs(30);
const SCHEDULE_FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(2);
const SCHEDULE_FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(60);
const SCHEDULE_OBSERVED_COLLECTIONS: &[&str] = &["Trigger", "Schedule"];

#[derive(Debug, Clone, Copy)]
struct FailureBackoff {
    failures: u32,
    retry_after: Instant,
}

#[derive(Debug, Default)]
struct ScheduleRetryState {
    backoffs: RapidHashMap<(String, String), FailureBackoff>,
    pending_advances: RapidHashMap<(String, String), DateTime<Utc>>,
}

impl ScheduleRetryState {
    fn clear_trigger_backoffs(&mut self, trigger_id: &str) {
        self.backoffs.retain(|(id, _), _| id != trigger_id);
    }

    fn prune_stale_for_trigger(&mut self, trigger_id: &str, current_next_run_at: &str) {
        self.backoffs
            .retain(|(id, next_run_at), _| id != trigger_id || next_run_at == current_next_run_at);
        self.pending_advances
            .retain(|(id, next_run_at), _| id != trigger_id || next_run_at == current_next_run_at);
    }
}

pub(crate) fn schedule_failure_backoff(failures: u32, base: Duration, max: Duration) -> Duration {
    let shift = failures.saturating_sub(1).min(30);
    let multiplier = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    base.saturating_mul(multiplier).min(max)
}

fn min_duration(current: Option<Duration>, candidate: Duration) -> Option<Duration> {
    Some(match current {
        Some(existing) => existing.min(candidate),
        None => candidate,
    })
}

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

pub struct ScheduleSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    snapshot_open: bool,
    node: Arc<EmbeddedNode>,
    subscription: Option<gents_store::subscription_source::ChangeSubscription>,
    subscription_initialized: bool,
    retry_state: Arc<Mutex<ScheduleRetryState>>,
    tick_every: Duration,
    failure_backoff_base: Duration,
    failure_backoff_max: Duration,
    cancel: CancellationToken,
    #[cfg(test)]
    idle_ticks_tx: Option<watch::Sender<usize>>,
}

impl ScheduleSource {
    pub fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            snapshot_open: true,
            node,
            subscription: None,
            subscription_initialized: false,
            retry_state: Arc::new(Mutex::new(ScheduleRetryState::default())),
            tick_every: SCHEDULE_FALLBACK_POLL,
            failure_backoff_base: SCHEDULE_FAILURE_BACKOFF_BASE,
            failure_backoff_max: SCHEDULE_FAILURE_BACKOFF_MAX,
            cancel,
            #[cfg(test)]
            idle_ticks_tx: None,
        }
    }

    fn ensure_subscription(&mut self) {
        if self.subscription_initialized {
            return;
        }
        self.subscription_initialized = true;
        match gents_store::subscription_source::subscribe_collections(
            &self.node,
            SCHEDULE_OBSERVED_COLLECTIONS.iter().copied(),
        ) {
            Ok(subscription) => self.subscription = Some(subscription),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "schedule source change subscription unavailable; using fallback poll"
                );
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn with_tick_every(mut self, tick_every: Duration) -> Self {
        self.tick_every = tick_every;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_failure_backoff(mut self, base: Duration, max: Duration) -> Self {
        self.failure_backoff_base = base;
        self.failure_backoff_max = max;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_idle_tick_signal(mut self, tx: watch::Sender<usize>) -> Self {
        self.idle_ticks_tx = Some(tx);
        self
    }
}

impl TriggerSource for ScheduleSource {
    fn next_fire(&mut self) -> Pin<Box<dyn Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            self.ensure_subscription();
            // `None` means "source permanently done, drop it"; an idle tick
            // must not exit, or the engine's outer loop kills the schedule
            // driver.
            loop {
                if self.cancel.is_cancelled() {
                    return None;
                }

                let snapshot = self.snapshot_rx.borrow().clone();
                let now = Utc::now();
                let now_instant = Instant::now();

                let schedules = snapshot
                    .active_schedules()
                    .iter()
                    .filter_map(|(trigger_id, resolved)| {
                        let node_did = if resolved.task.command.is_some() {
                            Some(snapshot.local_did.as_str())
                        } else {
                            snapshot
                                .agent(&resolved.task.agent_did)
                                .map(|behavior| behavior.node_did())
                        };
                        node_did.map(|node_did| (trigger_id, resolved, node_did))
                    })
                    .collect::<Vec<_>>();

                let mut earliest_wake: Option<Duration> = None;

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
                                earliest_wake = min_duration(earliest_wake, self.tick_every);
                                tracing::warn!(%error, "failed to observe cached Schedule visibility")
                            }
                        }
                    }
                    if visible.iter().any(|allowed| !allowed) {
                        earliest_wake = min_duration(earliest_wake, self.tick_every);
                    }
                    let schedule_chunk = schedule_chunk
                        .iter()
                        .copied()
                        .zip(visible)
                        .filter_map(|(schedule, visible)| visible.then_some(schedule))
                        .collect::<Vec<_>>();
                    if schedule_chunk.is_empty() {
                        continue;
                    }
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
                                    match gents_documents::automation::seed_schedule_next_run_at(
                                        &resolved.cadence,
                                        now,
                                    ) {
                                        Ok(next) => next,
                                        Err(e) => {
                                            earliest_wake =
                                                min_duration(earliest_wake, self.tick_every);
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
                                    earliest_wake = min_duration(earliest_wake, self.tick_every);
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
                                earliest_wake = min_duration(earliest_wake, self.tick_every);
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

                        let key = (trigger_id.clone(), next_run_at.clone());
                        let (pending_advance, backoff_until) = {
                            let mut state = self
                                .retry_state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            state.prune_stale_for_trigger(trigger_id, &next_run_at);
                            (
                                state.pending_advances.get(&key).copied(),
                                state.backoffs.get(&key).map(|entry| entry.retry_after),
                            )
                        };

                        if let Some(advanced_dt) = pending_advance {
                            let wait = (advanced_dt - now).to_std().unwrap_or_default();
                            earliest_wake =
                                min_duration(earliest_wake, wait.max(Duration::from_millis(10)));
                            continue;
                        }

                        if parsed > now {
                            let wait = (parsed - now).to_std().unwrap_or_default();
                            earliest_wake = min_duration(earliest_wake, wait);
                            continue;
                        }

                        if let Some(retry_after) = backoff_until {
                            if retry_after > now_instant {
                                earliest_wake =
                                    min_duration(earliest_wake, retry_after - now_instant);
                                continue;
                            }
                        }

                        let fired_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);
                        let event_vars = serde_json::json!({
                            "fired_at": fired_at,
                            "trigger_id": trigger_id,
                            "trigger_kind": "schedule",
                        });

                        let advanced_next_run_at =
                            match gents_documents::automation::advance_schedule_next_run_at(
                                &resolved.cadence,
                                parsed,
                                now,
                            ) {
                                Ok(next) => next,
                                Err(e) => {
                                    earliest_wake = min_duration(earliest_wake, self.tick_every);
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
                        let retry_state = self.retry_state.clone();
                        let backoff_base = self.failure_backoff_base;
                        let backoff_max = self.failure_backoff_max;
                        let occurrence_next_run_at = next_run_at.clone();

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
                            durable_fire_key:
                                gents_protocol::trigger_delivery::schedule_occurrence_key(
                                    trigger_id,
                                    &next_run_at,
                                ),
                            pre_materialized_request_id: None,
                            event_source_generation: None,
                            on_result: Box::new(move |result| {
                                let mut clear_pending_advance = false;
                                let updates = match &result {
                                    FireResult::Fired { request_id: id }
                                    | FireResult::CommandStarted { run_id: id } => {
                                        tracing::debug!(
                                            trigger_id = %trigger_id_for_callback,
                                            fired = %id,
                                            "schedule fire materialized"
                                        );
                                        clear_pending_advance = true;
                                        let mut state = retry_state
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        state.clear_trigger_backoffs(&trigger_id_for_callback);
                                        state.pending_advances.insert(
                                            (
                                                trigger_id_for_callback.clone(),
                                                occurrence_next_run_at.clone(),
                                            ),
                                            advanced_next_run_at,
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
                                    FireResult::Duplicate { .. } => {
                                        clear_pending_advance = true;
                                        let mut state = retry_state
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        state.clear_trigger_backoffs(&trigger_id_for_callback);
                                        state.pending_advances.insert(
                                            (
                                                trigger_id_for_callback.clone(),
                                                occurrence_next_run_at.clone(),
                                            ),
                                            advanced_next_run_at,
                                        );
                                        TriggerRuntimeUpdate {
                                            last_fired_source_doc_id: None,
                                            next_run_at: Some(advanced_next_run_at_str.clone()),
                                            last_attempt_at: Some(last_attempt_at.clone()),
                                            last_status: Some("duplicate".to_string()),
                                            last_error: None,
                                            fire_count_delta: None,
                                        }
                                    }
                                    FireResult::Skipped { .. } => {
                                        clear_pending_advance = true;
                                        let mut state = retry_state
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        state.clear_trigger_backoffs(&trigger_id_for_callback);
                                        state.pending_advances.insert(
                                            (
                                                trigger_id_for_callback.clone(),
                                                occurrence_next_run_at.clone(),
                                            ),
                                            advanced_next_run_at,
                                        );
                                        TriggerRuntimeUpdate {
                                            last_fired_source_doc_id: None,
                                            next_run_at: Some(advanced_next_run_at_str.clone()),
                                            last_attempt_at: Some(last_attempt_at.clone()),
                                            last_status: Some("skipped".to_string()),
                                            last_error: None,
                                            fire_count_delta: None,
                                        }
                                    }
                                    FireResult::Errored { error }
                                    | FireResult::Rejected { error } => {
                                        let mut state = retry_state
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        let entry = state
                                            .backoffs
                                            .entry((
                                                trigger_id_for_callback.clone(),
                                                occurrence_next_run_at.clone(),
                                            ))
                                            .or_insert(FailureBackoff {
                                                failures: 0,
                                                retry_after: Instant::now(),
                                            });
                                        entry.failures = entry.failures.saturating_add(1);
                                        let delay = schedule_failure_backoff(
                                            entry.failures,
                                            backoff_base,
                                            backoff_max,
                                        );
                                        entry.retry_after = Instant::now() + delay;
                                        TriggerRuntimeUpdate {
                                            last_fired_source_doc_id: None,
                                            next_run_at: None,
                                            last_attempt_at: Some(last_attempt_at.clone()),
                                            last_status: Some("error".to_string()),
                                            last_error: Some(error.clone()),
                                            fire_count_delta: None,
                                        }
                                    }
                                    FireResult::BudgetExhausted { reason } => {
                                        let mut state = retry_state
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        let entry = state
                                            .backoffs
                                            .entry((
                                                trigger_id_for_callback.clone(),
                                                occurrence_next_run_at.clone(),
                                            ))
                                            .or_insert(FailureBackoff {
                                                failures: 0,
                                                retry_after: Instant::now(),
                                            });
                                        entry.failures = entry.failures.saturating_add(1);
                                        let delay = schedule_failure_backoff(
                                            entry.failures,
                                            backoff_base,
                                            backoff_max,
                                        );
                                        entry.retry_after = Instant::now() + delay;
                                        TriggerRuntimeUpdate {
                                            last_fired_source_doc_id: None,
                                            next_run_at: None,
                                            last_attempt_at: Some(last_attempt_at.clone()),
                                            last_status: Some(
                                                gents_documents::BUDGET_EXHAUSTED_STATUS
                                                    .to_string(),
                                            ),
                                            last_error: Some(reason.clone()),
                                            fire_count_delta: None,
                                        }
                                    }
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
                                    if clear_pending_advance {
                                        let mut state = retry_state
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                                        state.pending_advances.remove(&(
                                            trigger_id_for_callback,
                                            occurrence_next_run_at,
                                        ));
                                    }
                                });
                            }),
                        });
                    }
                }

                let wait_for =
                    earliest_wake.map_or(self.tick_every, |duration| duration.min(self.tick_every));

                let timer_fired = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => return None,
                    changed = self.snapshot_rx.changed(), if self.snapshot_open => {
                        if changed.is_err() {
                            self.snapshot_open = false;
                        }
                        false
                    }
                    batch = async {
                        match self.subscription.as_mut() {
                            Some(subscription) => subscription.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if batch.is_none() {
                            self.subscription = None;
                        }
                        false
                    }
                    _ = tokio::time::sleep(wait_for) => true,
                };
                #[cfg(test)]
                if timer_fired {
                    if let Some(tx) = &self.idle_ticks_tx {
                        tx.send_modify(|ticks| *ticks += 1);
                    }
                }
                #[cfg(not(test))]
                let _ = timer_fired;
            }
        })
    }
}

#[cfg(test)]
mod tests;
