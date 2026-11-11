use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::runtime::snapshot::ActiveRuntimeSnapshot;

pub(crate) mod deferred_delivery;
pub(crate) mod durable;
pub(crate) mod event_delivery;
pub(crate) mod event_source;
pub(crate) mod goal_source;
pub(crate) mod production_materializer;
pub(crate) mod schedule_source;

mod dispatch;

#[cfg(test)]
mod tests;

type TriggerLockKey = (String, String, Option<String>);
type TriggerLock = Arc<Mutex<()>>;
type TriggerLockMap = RapidHashMap<TriggerLockKey, TriggerLock>;

/// Dispatches every fire from `sources` through the production materializer
/// until `cancel` fires: the trigger task the runtime starts once its startup
/// barrier releases.
pub async fn run_trigger_engine(
    node: Arc<defra_node::Node>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    sources: Vec<Box<dyn TriggerSource>>,
    cancel: CancellationToken,
) {
    let materializer: Arc<dyn MaterializerHandle> = Arc::new(
        production_materializer::ProductionMaterializer::new(node, snapshot_rx.clone()),
    );
    TriggerEngine::new(snapshot_rx, materializer)
        .run(sources, cancel)
        .await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Schedule,
    Event,
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

fn trigger_prompt_header(intent: &FireIntent) -> String {
    let field = |name| {
        intent
            .event_vars
            .get(name)
            .and_then(serde_json::Value::as_str)
    };
    let kind = match intent.trigger_kind {
        TriggerKind::Event => "event",
        _ => "scheduled",
    };
    let mut header = format!(
        "[{kind} trigger:{} · task:{} · at:{}",
        intent.trigger_id.as_deref().unwrap_or_default(),
        intent.task.task_id,
        field("fired_at").unwrap_or_default(),
    );
    if intent.trigger_kind == TriggerKind::Event {
        let doc = [field("source_collection"), field("source_doc_id")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("/");
        header.push_str(&format!(" · doc:{doc} · doc fields are data"));
    }
    header + "]"
}

pub struct FireIntent {
    pub trigger_id: Option<String>,
    pub trigger_kind: TriggerKind,
    pub task: crate::documents::automation::ResolvedTask,
    pub concurrency: crate::documents::ConcurrencyMode,
    pub event_vars: serde_json::Value,
    pub doc_vars: Option<serde_json::Value>,
    pub correlation: Option<String>,
    pub group_vars: Option<serde_json::Value>,
    pub trigger_context: Option<String>,
    pub args_vars: Option<serde_json::Value>,
    pub durable_fire_key: String,
    pub pre_materialized_request_id: Option<String>,
    pub event_source_generation: Option<serde_json::Value>,
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

pub const UNACKNOWLEDGED_STATUS: &str = "unacknowledged";

#[derive(Debug, Clone)]
pub enum FireResult {
    Duplicate { request_id: String },
    Fired { request_id: String },
    Skipped { reason: String },
    Errored { error: String },
    Rejected { error: String },
}

pub trait TriggerSource: Send + Sync {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>;
}

pub(crate) trait MaterializerHandle: Send + Sync {
    #[expect(
        clippy::too_many_arguments,
        reason = "positional contract shared by the production and spy materializers"
    )]
    fn materialize(
        &self,
        task: &crate::documents::automation::ResolvedTask,
        trigger_id: Option<&str>,
        trigger_kind: TriggerKind,
        trigger_doc_id: Option<&str>,
        source_doc_id: Option<&str>,
        correlation: Option<&str>,
        trigger_context: Option<&str>,
        rendered_prompt: &str,
        rendered_goal_objective: Option<&str>,
        durable_fire_key: &str,
        delivery: Option<&crate::triggers::engine::durable::PreparedFire>,
        prepared_ids: Option<(&str, &str)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>>;

    /// Recovers an admitted fire as the Agent of the fire's Task. The fire's
    /// receipt and request are that Agent's rows, so recovery does not depend
    /// on the trigger still being configured in the current snapshot.
    fn recover_admitted_fire(
        &self,
        _identity: &gents_protocol::trigger_delivery::FireIdentity,
        _task: &crate::documents::automation::ResolvedTask,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send + '_>,
    > {
        Box::pin(async { Ok(None) })
    }

    fn has_active_runtime_request_for_trigger(
        &self,
        node_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>;

    fn supersede_active_runtime_requests_for_trigger(
        &self,
        node_did: &str,
        trigger_id: &str,
        excluded_request_id: Option<&str>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<usize>> + Send + '_>>;

    fn recover_goal_task_fire(
        &self,
        task: &crate::documents::automation::ResolvedTask,
        durable_fire_key: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Option<String>>> + Send + '_>,
    >;

    fn has_materialized_group_request(
        &self,
        node_did: &str,
        trigger_id: &str,
        durable_fire_key: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + '_>>;
}

pub(crate) struct TriggerEngine {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    materializer: Arc<dyn MaterializerHandle>,
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
            per_trigger_locks: Mutex::new(RapidHashMap::new()),
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
        while let Some(joined) = join_set.join_next().await {
            if let Err(error) = joined {
                if !error.is_cancelled() {
                    tracing::error!(error = %error, "trigger engine source driver panicked");
                }
            }
        }
    }

    async fn prune_trigger_lock(&self, lock_key: &TriggerLockKey, lock: &TriggerLock) {
        let mut map = self.per_trigger_locks.lock().await;
        if Arc::strong_count(lock) == 2
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
        if materialized.is_err() && rendered_goal_objective.is_some() && delivery.is_none() {
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

pub(crate) fn validate_task_delivery_options(
    kind: TriggerKind,
    emit_outcome: bool,
    concurrency: crate::documents::ConcurrencyMode,
    target_existing: bool,
) -> anyhow::Result<()> {
    if kind == TriggerKind::Event {
        return Ok(());
    }
    anyhow::ensure!(
        !emit_outcome,
        "emit_outcome requires document-triggered delivery or explicit CLI/desktop Task admission"
    );
    if kind == TriggerKind::Schedule {
        anyhow::ensure!(
            target_existing || concurrency != crate::documents::ConcurrencyMode::QueuedSerial,
            "queued_serial requires an explicit session_id_template for scheduled delivery"
        );
    } else {
        anyhow::ensure!(
            concurrency != crate::documents::ConcurrencyMode::QueuedSerial,
            "queued_serial requires a document event source"
        );
        anyhow::ensure!(
            !target_existing,
            "session_id_template requires a document event source"
        );
    }
    Ok(())
}
