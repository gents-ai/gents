//! Trigger engine scaffold.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;

pub(crate) mod cross_deployment_cancel_mirror;
pub(crate) mod event_source;
pub(crate) mod goal_source;
pub(crate) mod manual_source;
pub(crate) mod production_materializer;
pub(crate) mod schedule_source;
pub(crate) mod subagent_source;
pub mod subscription_source;

#[cfg(test)]
mod tests;

type TriggerLockKey = (String, String, TriggerKind, Option<String>);
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

pub(crate) fn fire_result_from_materialize(result: anyhow::Result<String>) -> FireResult {
    match result {
        Ok(request_id) => FireResult::Fired { request_id },
        Err(error) => {
            if let Some(skip) = error.downcast_ref::<MaterializeSkip>() {
                FireResult::Skipped {
                    reason: skip.reason.clone(),
                }
            } else {
                FireResult::Errored {
                    error: format!("materialize: {error}"),
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum FireResult {
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>>;

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
        trigger_kind: TriggerKind,
        correlation: Option<&str>,
        excluded_request_id: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>;

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        agent_did: &str,
        trigger_id: &str,
        trigger_kind: TriggerKind,
        correlation: Option<&str>,
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
        trigger_kind: TriggerKind,
        correlation: &str,
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
            let result = FireResult::Errored {
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
                    let result = FireResult::Errored {
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
                    let result = FireResult::Errored {
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

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let (node_scope, ctx_scope) =
            crate::template::task_node_ctx(&snapshot.local_did, &intent.task.behavior_id, &now);
        let scope = crate::template::TemplateScope {
            event: intent.event_vars.clone(),
            doc: intent.doc_vars.clone(),
            args: intent.args_vars.clone(),
            group: intent.group_vars.clone(),
            node: node_scope,
            ctx: ctx_scope,
        };
        let rendered = match crate::template::render_template(&intent.task.prompt_template, &scope)
        {
            Ok(s) => s,
            Err(e) => {
                let result = FireResult::Errored {
                    error: format!("template: {e}"),
                };
                (intent.on_result)(result.clone());
                return result;
            }
        };
        let rendered_goal_objective = match intent.task.goal_objective_template.as_deref() {
            Some(template) => match crate::template::render_template(template, &scope) {
                Ok(objective) => Some(objective),
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("goal template: {error}"),
                    };
                    (intent.on_result)(result.clone());
                    return result;
                }
            },
            None if intent.task.goal_token_budget.is_some() => {
                let result = FireResult::Errored {
                    error: "goal token budget requires a goal objective template".to_string(),
                };
                (intent.on_result)(result.clone());
                return result;
            }
            None => None,
        };
        if let Err(error) = crate::goal::validate_task_goal_declaration(
            rendered_goal_objective.as_deref(),
            intent.task.goal_token_budget,
        ) {
            let result = FireResult::Errored {
                error: format!("goal declaration: {error}"),
            };
            (intent.on_result)(result.clone());
            return result;
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
                .materialize_after_lock(intent, trigger_doc_id, rendered, rendered_goal_objective)
                .await;
        };
        if intent.concurrency == crate::runtime_snapshot::ConcurrencyMode::Parallel
            && intent.group_vars.is_none()
        {
            return self
                .materialize_after_lock(intent, trigger_doc_id, rendered, rendered_goal_objective)
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
        // Correlation scopes concurrency only for an actual group fire. A
        // per-document trigger may carry correlation for lineage and fills,
        // but Serial/LatestOnly remain trigger-wide as modeled in Lean.
        let concurrency_correlation = intent
            .group_vars
            .as_ref()
            .and_then(|_| intent.correlation.clone());
        let lock_key = (
            agent_did.clone(),
            trigger_id.clone(),
            intent.trigger_kind,
            concurrency_correlation.clone(),
        );
        let lock = {
            let mut map = self.per_trigger_locks.lock().await;
            map.entry(lock_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;

        if intent.group_vars.is_some() {
            let Some(correlation) = intent
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
                .has_materialized_group_request(
                    &agent_did,
                    &trigger_id,
                    intent.trigger_kind,
                    correlation,
                )
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
        match intent.concurrency {
            ConcurrencyMode::Parallel => {}
            ConcurrencyMode::Serial => match self
                .materializer
                .has_active_runtime_request_for_trigger(
                    &agent_did,
                    &trigger_id,
                    intent.trigger_kind,
                    concurrency_correlation.as_deref(),
                    durable_goal_request_id.as_deref(),
                )
                .await
            {
                Ok(true) => {
                    let result = FireResult::Skipped {
                        reason: "serial: prior fire still in-flight".to_string(),
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
            ConcurrencyMode::LatestOnly => {
                if let Err(error) = self
                    .materializer
                    .supersede_active_runtime_requests_for_trigger(
                        &agent_did,
                        &trigger_id,
                        intent.trigger_kind,
                        concurrency_correlation.as_deref(),
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
            .materialize_after_lock(intent, trigger_doc_id, rendered, rendered_goal_objective)
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

#[doc(hidden)]
pub async fn run_subagent_source_for_test(
    node: Arc<defra_node::EmbeddedNode>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    authorized_peer_dids: std::collections::HashSet<String>,
    cancel: CancellationToken,
) {
    struct UnusedMaterializer;
    impl MaterializerHandle for UnusedMaterializer {
        fn materialize(
            &self,
            _task: &crate::runtime_snapshot::ResolvedTask,
            _trigger_id: Option<&str>,
            _trigger_kind: TriggerKind,
            _trigger_doc_id: Option<&str>,
            _source_doc_id: Option<&str>,
            _correlation: Option<&str>,
            _trigger_context: Option<&str>,
            _rendered_prompt: &str,
            _rendered_goal_objective: Option<&str>,
            _durable_fire_key: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>>
        {
            Box::pin(async {
                unreachable!(
                    "SubagentSource fires pre-materialized requests; materialize is never called"
                )
            })
        }

        fn has_active_runtime_request_for_trigger(
            &self,
            _agent_did: &str,
            _trigger_id: &str,
            _trigger_kind: TriggerKind,
            _correlation: Option<&str>,
            _excluded_request_id: Option<&str>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>
        {
            Box::pin(async { Ok(false) })
        }

        fn supersede_active_runtime_requests_for_trigger(
            &self,
            _agent_did: &str,
            _trigger_id: &str,
            _trigger_kind: TriggerKind,
            _correlation: Option<&str>,
            _excluded_request_id: Option<&str>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<usize>> + Send + '_>>
        {
            Box::pin(async { Ok(0) })
        }

        fn recover_goal_task_fire(
            &self,
            _task: &crate::runtime_snapshot::ResolvedTask,
            _durable_fire_key: &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send + '_>,
        > {
            Box::pin(async { Ok(None) })
        }

        fn has_materialized_group_request(
            &self,
            _agent_did: &str,
            _trigger_id: &str,
            _trigger_kind: TriggerKind,
            _correlation: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>
        {
            Box::pin(async { Ok(false) })
        }
    }

    let subagent_source: Box<dyn TriggerSource> = Box::new(
        subagent_source::SubagentSource::with_subscription_source_for_test(
            node.clone(),
            snapshot_rx.clone(),
            node,
            authorized_peer_dids,
            cancel.clone(),
        ),
    );
    let materializer: Arc<dyn MaterializerHandle> = Arc::new(UnusedMaterializer);
    let engine = TriggerEngine::new(snapshot_rx, materializer);
    engine.run(vec![subagent_source], cancel).await;
}
