//! Trigger engine scaffold.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;

pub(crate) mod deferred_delivery;
pub(crate) mod durable;
pub(crate) mod event_delivery;
pub(crate) mod event_source;
pub(crate) mod goal_source;
pub(crate) mod manual_source;
pub(crate) mod production_materializer;
pub(crate) mod schedule_source;
pub mod subscription_source;

#[cfg(test)]
mod tests;

// Per-document fires share the canonical owner/trigger gate; grouped fires
// additionally select the complete typed group generation through its durable key.
type TriggerLockKey = (String, String, Option<String>);
type TriggerLock = Arc<Mutex<()>>;
type TriggerLockMap = HashMap<TriggerLockKey, TriggerLock>;

pub(crate) fn durable_fire_key(namespace: &str, components: &[&str]) -> String {
    let mut key = format!("{}:{namespace}", namespace.chars().count());
    for component in components {
        key.push_str(&format!(":{}:{component}", component.chars().count()));
    }
    key
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Schedule,
    #[allow(dead_code)]
    Event,
    #[allow(dead_code)]
    Manual,
}

impl TriggerKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Schedule => "schedule",
            TriggerKind::Event => "event",
            TriggerKind::Manual => "manual",
        }
    }
}

pub struct FireIntent {
    pub trigger_id: Option<String>,
    pub trigger_kind: TriggerKind,
    pub task: crate::runtime_snapshot::ResolvedTask,
    pub concurrency: crate::runtime_snapshot::ConcurrencyMode,
    pub event_vars: serde_json::Value,
    pub doc_vars: Option<serde_json::Value>,
    pub correlation: Option<String>,
    pub group_vars: Option<serde_json::Value>,
    pub trigger_context: Option<String>,
    pub args_vars: Option<serde_json::Value>,
    /// Stable identity of this logical source delivery. Goal-backed Tasks use
    /// it with `task_id` to recover the same session/request after retries.
    pub durable_fire_key: String,
    pub pre_materialized_request_id: Option<String>,
    pub on_result: Box<dyn FnOnce(FireResult) + Send>,
}

impl FireIntent {
    fn well_formed_error(&self) -> Option<&'static str> {
        if self.durable_fire_key.trim().is_empty() {
            return Some("Trigger fire intent must carry a durable fire key");
        }
        if self.group_vars.is_some() && !event_delivery::is_group_fire_key(&self.durable_fire_key) {
            return Some("Grouped trigger intent requires its canonical event-group fire key");
        }
        match self.trigger_kind {
            TriggerKind::Manual if self.trigger_id.is_some() => {
                Some("Manual trigger intent must not carry trigger_id")
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MaterializeSkip {
    pub reason: String,
}

impl std::fmt::Display for MaterializeSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for MaterializeSkip {}

#[derive(Debug, thiserror::Error)]
#[error("fire already admitted as {request_id}")]
pub(crate) struct MaterializeDuplicate {
    pub request_id: String,
}

pub(crate) fn fire_result_from_materialize(result: anyhow::Result<String>) -> FireResult {
    match result {
        Ok(request_id) => FireResult::Fired { request_id },
        Err(error) => {
            if let Some(skip) = error.downcast_ref::<MaterializeSkip>() {
                FireResult::Skipped {
                    reason: skip.reason.clone(),
                }
            } else if let Some(duplicate) = error.downcast_ref::<MaterializeDuplicate>() {
                FireResult::Duplicate {
                    request_id: duplicate.request_id.clone(),
                }
            } else {
                FireResult::Errored {
                    error: format!("materialize: {error}"),
                }
            }
        }
    }
}

pub(crate) const SERIAL_BUSY: &str = "serial: prior fire still in-flight";

/// Recorded when a fire's result never reaches the acknowledgment channel, so
/// the park's own `on_result` writer cannot run. The eval runner reads this
/// status and classes the slot as a harness fault instead of waiting out the
/// stage deadline on no evidence.
pub(crate) const UNACKNOWLEDGED_STATUS: &str = "unacknowledged";

#[derive(Debug, Clone)]
pub enum FireResult {
    Duplicate {
        request_id: String,
    },
    #[allow(dead_code)]
    Fired {
        request_id: String,
    },
    #[allow(dead_code)]
    Skipped {
        reason: String,
    },
    Errored {
        error: String,
    },
    /// The intent cannot be admitted under the configuration snapshot it was
    /// dispatched against: rendering or preparing it failed on the Task,
    /// Trigger or source document alone, so repeating it under the same
    /// snapshot repeats the refusal. Nothing was admitted; an event arrival
    /// stays pending (`EventDelivery.Durable.unadmitted_prefix_match_cannot_advance`)
    /// and its source retries it only after the configuration changes.
    Rejected {
        error: String,
    },
}

pub trait TriggerSource: Send + Sync {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>;
}

pub(crate) trait MaterializerHandle: Send + Sync {
    fn materialize(
        &self,
        task: &crate::runtime_snapshot::ResolvedTask,
        trigger_id: Option<&str>,
        trigger_kind: TriggerKind,
        trigger_doc_id: Option<&str>,
        source_doc_id: Option<&str>,
        correlation: Option<&str>,
        trigger_context: Option<&str>,
        rendered_prompt: &str,
        rendered_goal_objective: Option<&str>,
        durable_fire_key: &str,
        delivery: Option<&crate::trigger_engine::durable::PreparedFire>,
        prepared_ids: Option<(&str, &str)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>>;

    fn recover_event_fire(
        &self,
        _identity: &gents_protocol::trigger_delivery::FireIdentity,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send + '_>,
    > {
        Box::pin(async { Ok(None) })
    }

    fn resolve_graph_session(
        &self,
        _task: &crate::runtime_snapshot::ResolvedTask,
        _trigger_id: &str,
        _correlation: Option<&str>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send + '_>,
    > {
        Box::pin(async { Ok(None) })
    }

    /// Check whether any active runtime `AgentRequest` of `agent_did` is
    /// currently bound to this trigger. Used by the concurrency gate to
    /// decide whether a new fire should skip or supersede.
    ///
    /// The DID scope is load-bearing: on a replicated fleet the store also
    /// holds OTHER agents' requests for the same human-chosen trigger id, and
    /// those must never gate this agent's fires (#605).
    fn has_active_runtime_request_for_trigger(
        &self,
        agent_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>;

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        agent_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<usize>> + Send + '_>>;

    fn recover_goal_task_fire(
        &self,
        task: &crate::runtime_snapshot::ResolvedTask,
        durable_fire_key: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send + '_>,
    >;

    fn has_materialized_group_request(
        &self,
        agent_did: &str,
        trigger_id: &str,
        durable_fire_key: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>;
}

/// Scaffolding for the trigger engine.
///
/// The engine owns a read handle onto the active runtime snapshot (to look up
/// behaviors / concurrency / enabled gates at fire time) and a materializer
/// handle (to create requests). Per-trigger mutexes serialize dispatches that
/// share a trigger id so the concurrency gate and request materialization
/// land atomically with respect to each other.
pub(crate) struct TriggerEngine {
    #[allow(dead_code)]
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    #[allow(dead_code)]
    materializer: Arc<dyn MaterializerHandle>,
    #[allow(dead_code)]
    per_trigger_locks: Mutex<TriggerLockMap>,
}

impl TriggerEngine {
    pub(crate) fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        materializer: Arc<dyn MaterializerHandle>,
    ) -> Self {
        Self {
            snapshot_rx,
            materializer,
            per_trigger_locks: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn run(self, sources: Vec<Box<dyn TriggerSource>>, cancel: CancellationToken) {
        let engine = Arc::new(self);
        let mut join_set = tokio::task::JoinSet::new();
        for mut source in sources {
            let engine = engine.clone();
            let cancel = cancel.clone();
            join_set.spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        intent = source.next_fire() => {
                            match intent {
                                Some(intent) => {
                                    let _ = engine.dispatch(intent).await;
                                }
                                None => break,
                            }
                        }
                    }
                }
            });
        }
        // Wait for all source drivers to terminate (either via cancel or
        // their source returning `None`). Any panic in a driver task is
        // logged — individual source failures must not bring down the
        // entire engine.
        while let Some(joined) = join_set.join_next().await {
            if let Err(error) = joined {
                if !error.is_cancelled() {
                    tracing::error!(error = %error, "trigger engine source driver panicked");
                }
            }
        }
    }

    #[allow(dead_code)]
    async fn dispatch(&self, intent: FireIntent) -> FireResult {
        if let Some(error) = intent.well_formed_error() {
            let result = FireResult::Rejected {
                error: error.to_string(),
            };
            (intent.on_result)(result.clone());
            return result;
        }

        if let Some(request_id) = intent.pre_materialized_request_id.clone() {
            let result = FireResult::Fired { request_id };
            (intent.on_result)(result.clone());
            return result;
        }

        let event_identity = if intent.trigger_kind == TriggerKind::Event {
            let snapshot = self.snapshot_rx.borrow().clone();
            let collection = intent
                .event_vars
                .get("source_collection")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    snapshot
                        .active_event_triggers()
                        .get(intent.trigger_id.as_deref().unwrap_or_default())
                        .map(|trigger| trigger.source_collection.as_str())
                });
            let (collection, document) = match &intent.group_vars {
                Some(group) => (
                    Some("EventGroupState"),
                    group
                        .get("state_doc_id")
                        .and_then(serde_json::Value::as_str),
                ),
                None => (
                    collection,
                    intent
                        .event_vars
                        .get("source_doc_id")
                        .and_then(serde_json::Value::as_str),
                ),
            };
            match (intent.trigger_id.as_ref(), collection, document) {
                (Some(trigger), Some(collection), Some(document)) => {
                    Some(gents_protocol::trigger_delivery::FireIdentity {
                        owner_did: snapshot.local_did.clone(),
                        trigger_id: trigger.clone(),
                        source_collection: collection.to_owned(),
                        source_doc_id: document.to_owned(),
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(identity) = &event_identity {
            match self.materializer.recover_event_fire(identity).await {
                Ok(Some(request_id)) => {
                    let result = FireResult::Duplicate { request_id };
                    (intent.on_result)(result.clone());
                    return result;
                }
                Ok(None) => {}
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("recover event fire: {error}"),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
        }

        // Recovery is keyed by the durable Task/fire identity, not by the
        // Task's current declaration. The goal, request, and claim may have
        // committed before the source checkpoint did; if an operator removes
        // the declaration before restart, that exact fire must still recover
        // instead of falling through to a second ordinary request.
        match self
            .materializer
            .recover_goal_task_fire(&intent.task, &intent.durable_fire_key)
            .await
        {
            Ok(Some(request_id)) => {
                let result = FireResult::Fired { request_id };
                (intent.on_result)(result.clone());
                return result;
            }
            Ok(None) => {}
            Err(error) => {
                let result = FireResult::Errored {
                    error: format!("recover durable goal Task fire: {error}"),
                };
                (intent.on_result)(result.clone());
                return result;
            }
        }

        let snapshot = self.snapshot_rx.borrow().clone();

        let trigger_doc_id = match intent.trigger_kind {
            TriggerKind::Schedule => {
                let Some(trigger_id) = intent.trigger_id.as_deref() else {
                    let result = FireResult::Rejected {
                        error: "Schedule trigger missing trigger_id".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                let Some(trigger) = snapshot.active_schedules().get(trigger_id) else {
                    let result = FireResult::Skipped {
                        reason: "trigger disabled".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                Some(trigger.trigger_doc_id.clone())
            }
            TriggerKind::Event => {
                let Some(trigger_id) = intent.trigger_id.as_deref() else {
                    let result = FireResult::Rejected {
                        error: "Event trigger missing trigger_id".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                let Some(trigger) = snapshot.active_event_triggers().get(trigger_id) else {
                    let result = FireResult::Skipped {
                        reason: "trigger disabled".to_string(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                Some(trigger.trigger_doc_id.clone())
            }
            TriggerKind::Manual => None,
        };

        let graph_session_id = if let Some(trigger_id) = intent.trigger_id.as_deref() {
            match self
                .materializer
                .resolve_graph_session(&intent.task, trigger_id, intent.correlation.as_deref())
                .await
            {
                Ok(session) => session,
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("resolve graph session: {error}"),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
        } else {
            None
        };

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let (node_scope, ctx_scope) =
            crate::template::task_node_ctx(&snapshot.local_did, &intent.task.behavior_id, &now);
        if intent.trigger_kind != TriggerKind::Event {
            let unsupported_session = intent
                .trigger_id
                .as_deref()
                .and_then(|id| snapshot.active_schedules().get(id))
                .is_some_and(|schedule| schedule.session_id_template.is_some());
            if let Err(error) = validate_non_document_task_options(
                intent.task.emit_outcome,
                intent.concurrency,
                unsupported_session,
            ) {
                let result = FireResult::Rejected {
                    error: error.to_string(),
                };
                (intent.on_result)(result.clone());
                return result;
            }
        }
        let prepared_ids = if matches!(
            intent.trigger_kind,
            TriggerKind::Manual | TriggerKind::Schedule
        ) {
            Some(if intent.task.goal_objective_template.is_some() {
                let Some(behavior) = snapshot.behavior(&intent.task.behavior_id) else {
                    let result = FireResult::Errored {
                        error: "Task behavior unavailable".into(),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                };
                let identity = crate::goal::task_goal_fire_identity(
                    behavior.agent_did(),
                    &intent.task.task_id,
                    &intent.durable_fire_key,
                );
                (identity.request_id, identity.session_id)
            } else {
                (
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                )
            })
        } else {
            None
        };
        let mut scope = crate::template::TemplateScope {
            session: prepared_ids
                .as_ref()
                .map(|(_, session)| serde_json::json!({"session_id": session})),
            request: prepared_ids
                .as_ref()
                .map(|(request, _)| serde_json::json!({"request_id": request})),
            event: intent.event_vars.clone(),
            doc: intent.doc_vars.clone(),
            args: intent.args_vars.clone(),
            group: intent.group_vars.clone(),
            node: node_scope,
            ctx: ctx_scope,
        };
        if intent.trigger_kind == TriggerKind::Event
            && snapshot.behavior(&intent.task.behavior_id).is_none()
        {
            let result = FireResult::Errored {
                error: "prepare fire: trigger behavior unavailable".into(),
            };
            (intent.on_result)(result.clone());
            return result;
        }
        let mut delivery = if intent.trigger_kind == TriggerKind::Event {
            let prepared = (|| -> anyhow::Result<gents_protocol::trigger_delivery::TriggerFire> {
                let trigger = snapshot
                    .active_event_triggers()
                    .get(intent.trigger_id.as_deref().unwrap_or_default())
                    .ok_or_else(|| anyhow::anyhow!("event trigger disappeared"))?;
                let owner = snapshot
                    .behavior(&intent.task.behavior_id)
                    .ok_or_else(|| anyhow::anyhow!("trigger behavior unavailable"))?
                    .agent_did()
                    .to_string();
                let (source_collection, source_doc_id) = if let Some(group) = &intent.group_vars {
                    (
                        "EventGroupState",
                        group
                            .get("state_doc_id")
                            .and_then(serde_json::Value::as_str),
                    )
                } else {
                    (
                        trigger.source_collection.as_str(),
                        intent
                            .event_vars
                            .get("source_doc_id")
                            .and_then(serde_json::Value::as_str),
                    )
                };
                let source_doc_id = source_doc_id
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("fire lacks source document ID"))?;
                let identity = gents_protocol::trigger_delivery::FireIdentity {
                    owner_did: owner,
                    trigger_id: trigger.trigger_id.clone(),
                    source_collection: source_collection.into(),
                    source_doc_id: source_doc_id.into(),
                };
                let key = durable::fire_key(&identity);
                let session_id = match trigger.session_id_template.as_deref() {
                    Some(template) => {
                        let value = crate::template::render_template(template, &scope)?;
                        anyhow::ensure!(
                            !value.trim().is_empty(),
                            "session template rendered an empty ID"
                        );
                        anyhow::ensure!(
                            graph_session_id
                                .as_ref()
                                .is_none_or(|session| session == &value),
                            "trigger session template disagrees with pinned graph session"
                        );
                        value
                    }
                    None => graph_session_id
                        .clone()
                        .unwrap_or_else(|| identity.session_id()),
                };
                let source_string = |field: &str| {
                    intent
                        .doc_vars
                        .as_ref()
                        .and_then(|d| d.get(field))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                let fire = gents_protocol::trigger_delivery::TriggerFire {
                    request_id: identity.request_id(),
                    fire_key: key,
                    identity,
                    goal_id: intent.task.goal_objective_template.as_ref().map(|_| {
                        crate::goal::deterministic_goal_id(
                            snapshot
                                .behavior(&intent.task.behavior_id)
                                .unwrap()
                                .agent_did(),
                            &session_id,
                        )
                    }),
                    session_id,
                    task_id: intent.task.task_id.clone(),
                    emit_outcome: intent.task.emit_outcome,
                    goal_objective: None,
                    goal_token_budget: intent.task.goal_token_budget,
                    goal_assignment_applied: false,
                    queued_serial: intent.concurrency
                        == crate::runtime_snapshot::ConcurrencyMode::QueuedSerial,
                    source_handoff_id: source_string("handoff_id"),
                    reply_session_id: source_string("reply_session_id"),
                    shard_id: source_string("shard_id"),
                    attempt: intent
                        .doc_vars
                        .as_ref()
                        .and_then(|d| d.get("attempt"))
                        .and_then(serde_json::Value::as_i64),
                    created_at: now.clone(),
                };
                anyhow::ensure!(
                    !fire.emit_outcome
                        || fire
                            .source_handoff_id
                            .as_ref()
                            .is_some_and(|s| !s.is_empty()),
                    "emit_outcome requires a source handoff_id"
                );
                Ok(fire)
            })();
            match prepared {
                Ok(fire) => {
                    scope.session = Some(serde_json::json!({"session_id": fire.session_id}));
                    scope.request = Some(serde_json::json!({"request_id": fire.request_id}));
                    Some(durable::PreparedFire {
                        target_existing: graph_session_id.is_some()
                            || snapshot
                                .active_event_triggers()
                                .get(intent.trigger_id.as_deref().unwrap())
                                .is_some_and(|t| t.session_id_template.is_some()),
                        receipt: fire,
                    })
                }
                Err(error) => {
                    let result = FireResult::Rejected {
                        error: format!("prepare fire: {error}"),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
        } else {
            None
        };
        let (rendered, rendered_goal_objective) = match crate::template::render_task(
            &intent.task.prompt_template,
            intent.task.goal_objective_template.as_deref(),
            intent.task.goal_token_budget,
            &scope,
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                let result = FireResult::Rejected { error };
                (intent.on_result)(result.clone());
                return result;
            }
        };
        if let Some(delivery) = &mut delivery {
            delivery.receipt.goal_objective = rendered_goal_objective.clone();
        }
        let concurrency_agent_did = || {
            snapshot
                .behavior(&intent.task.behavior_id)
                .map(|behavior| behavior.agent_did().to_string())
                .ok_or_else(|| {
                    snapshot
                        .unavailable_public_message(&intent.task.behavior_id)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| {
                            format!("behavior {} is not loaded", intent.task.behavior_id)
                        })
                })
        };
        let durable_goal_request_id = match rendered_goal_objective.as_ref() {
            Some(_) => match concurrency_agent_did() {
                Ok(agent_did) => Some(
                    crate::goal::task_goal_fire_identity(
                        &agent_did,
                        &intent.task.task_id,
                        &intent.durable_fire_key,
                    )
                    .request_id,
                ),
                Err(reason) => {
                    let result = FireResult::Errored {
                        error: format!("goal identity: {reason}"),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            },
            None => None,
        };
        let Some(trigger_id) = intent.trigger_id.clone() else {
            return self
                .materialize_after_lock(
                    intent,
                    trigger_doc_id,
                    rendered,
                    rendered_goal_objective,
                    delivery,
                    prepared_ids,
                )
                .await;
        };
        if intent.concurrency == crate::runtime_snapshot::ConcurrencyMode::Parallel
            && intent.group_vars.is_none()
        {
            return self
                .materialize_after_lock(
                    intent,
                    trigger_doc_id,
                    rendered,
                    rendered_goal_objective,
                    delivery,
                    prepared_ids,
                )
                .await;
        }
        let agent_did = match concurrency_agent_did() {
            Ok(did) => did,
            Err(reason) => {
                let result = FireResult::Errored {
                    error: format!("concurrency gate: {reason}"),
                };
                (intent.on_result)(result.clone());
                return result;
            }
        };
        let lock_key = (
            agent_did.clone(),
            trigger_id.clone(),
            intent
                .group_vars
                .as_ref()
                .map(|_| intent.durable_fire_key.clone()),
        );
        let lock = {
            let mut map = self.per_trigger_locks.lock().await;
            map.entry(lock_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;

        if let Some(identity) = &event_identity {
            let recovered = self.materializer.recover_event_fire(identity).await;
            let result = match recovered {
                Ok(Some(request_id)) => Some(FireResult::Duplicate { request_id }),
                Ok(None) => None,
                Err(error) => Some(FireResult::Errored {
                    error: format!("recover locked event fire: {error}"),
                }),
            };
            if let Some(result) = result {
                drop(guard);
                self.prune_trigger_lock(&lock_key, &lock).await;
                (intent.on_result)(result.clone());
                return result;
            }
        }

        if intent.group_vars.is_some() {
            let Some(_correlation) = intent
                .correlation
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                let result = FireResult::Errored {
                    error: "per_group intent requires a non-empty correlation".to_string(),
                };
                drop(guard);
                self.prune_trigger_lock(&lock_key, &lock).await;
                (intent.on_result)(result.clone());
                return result;
            };
            match self
                .materializer
                .has_materialized_group_request(&agent_did, &trigger_id, &intent.durable_fire_key)
                .await
            {
                Ok(true) => {
                    let result = FireResult::Skipped {
                        reason: "per_group: request already materialized".to_string(),
                    };
                    drop(guard);
                    self.prune_trigger_lock(&lock_key, &lock).await;
                    (intent.on_result)(result.clone());
                    return result;
                }
                Ok(false) => {}
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("group marker query: {error}"),
                    };
                    drop(guard);
                    self.prune_trigger_lock(&lock_key, &lock).await;
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
        }

        use crate::runtime_snapshot::ConcurrencyMode;
        // Every request, including active work, is a group marker. Under the
        // full group lock, an absent marker leaves no grouped request to gate
        // or supersede. Per-document concurrency remains trigger-wide.
        match (intent.group_vars.is_some(), intent.concurrency) {
            (true, _) | (false, ConcurrencyMode::Parallel | ConcurrencyMode::QueuedSerial) => {}
            (false, ConcurrencyMode::Serial) => match self
                .materializer
                .has_active_runtime_request_for_trigger(
                    &agent_did,
                    &trigger_id,
                    durable_goal_request_id.as_deref(),
                )
                .await
            {
                Ok(true) => {
                    let result = FireResult::Skipped {
                        reason: SERIAL_BUSY.to_string(),
                    };
                    drop(guard);
                    self.prune_trigger_lock(&lock_key, &lock).await;
                    (intent.on_result)(result.clone());
                    return result;
                }
                Ok(false) => {}
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("in-flight query: {error}"),
                    };
                    drop(guard);
                    self.prune_trigger_lock(&lock_key, &lock).await;
                    (intent.on_result)(result.clone());
                    return result;
                }
            },
            (false, ConcurrencyMode::LatestOnly) => {
                if let Err(error) = self
                    .materializer
                    .supersede_active_runtime_requests_for_trigger(
                        &agent_did,
                        &trigger_id,
                        durable_goal_request_id.as_deref(),
                    )
                    .await
                {
                    let result = FireResult::Errored {
                        error: format!("supersede: {error}"),
                    };
                    drop(guard);
                    self.prune_trigger_lock(&lock_key, &lock).await;
                    (intent.on_result)(result.clone());
                    return result;
                }
            }
        }

        let result = self
            .materialize_after_lock(
                intent,
                trigger_doc_id,
                rendered,
                rendered_goal_objective,
                delivery,
                prepared_ids,
            )
            .await;
        drop(guard);
        self.prune_trigger_lock(&lock_key, &lock).await;
        result
    }

    async fn prune_trigger_lock(&self, lock_key: &TriggerLockKey, lock: &TriggerLock) {
        let mut map = self.per_trigger_locks.lock().await;
        if Arc::strong_count(&lock) == 2
            && map
                .get(lock_key)
                .is_some_and(|stored| Arc::ptr_eq(stored, lock))
        {
            map.remove(lock_key);
        }
    }

    async fn materialize_after_lock(
        &self,
        intent: FireIntent,
        trigger_doc_id: Option<String>,
        rendered: String,
        rendered_goal_objective: Option<String>,
        delivery: Option<durable::PreparedFire>,
        prepared_ids: Option<(String, String)>,
    ) -> FireResult {
        let source_doc_id = if matches!(intent.trigger_kind, TriggerKind::Event) {
            intent
                .event_vars
                .get("source_doc_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        let mut materialized = self
            .materializer
            .materialize(
                &intent.task,
                intent.trigger_id.as_deref(),
                intent.trigger_kind,
                trigger_doc_id.as_deref(),
                source_doc_id.as_deref(),
                intent.correlation.as_deref(),
                intent.trigger_context.as_deref(),
                &rendered,
                rendered_goal_objective.as_deref(),
                &intent.durable_fire_key,
                delivery.as_ref(),
                prepared_ids
                    .as_ref()
                    .map(|(request, session)| (request.as_str(), session.as_str())),
            )
            .await;
        if materialized.is_err() && rendered_goal_objective.is_some() {
            if let Ok(Some(request_id)) = self
                .materializer
                .recover_goal_task_fire(&intent.task, &intent.durable_fire_key)
                .await
            {
                materialized = Ok(request_id);
            }
        }
        let result = fire_result_from_materialize(materialized);
        (intent.on_result)(result.clone());
        result
    }
}

pub(crate) fn validate_non_document_task_options(
    emit_outcome: bool,
    concurrency: crate::runtime_snapshot::ConcurrencyMode,
    session_template: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !emit_outcome,
        "emit_outcome requires document-triggered delivery or explicit CLI/desktop Task admission"
    );
    anyhow::ensure!(
        concurrency != crate::runtime_snapshot::ConcurrencyMode::QueuedSerial,
        "queued_serial requires a document event source"
    );
    anyhow::ensure!(
        !session_template,
        "session_id_template requires a document event source"
    );
    Ok(())
}
