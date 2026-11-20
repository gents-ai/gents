use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use gents_documents::task_delivery::TriggerKind;

pub mod command;
pub(crate) mod deferred_delivery;
pub mod durable;
pub(crate) mod event_delivery;
pub mod event_source;
pub mod goal_source;
pub mod production_materializer;
pub mod schedule_source;

mod dispatch;

#[cfg(test)]
mod tests;

type TriggerLockKey = (String, String, Option<String>);
type TriggerLock = Arc<Mutex<()>>;
type TriggerLockMap = RapidHashMap<TriggerLockKey, TriggerLock>;

/// Dispatches every fire from `sources` through the production materializer
/// until `cancel` fires: the trigger task the runtime starts once its startup
/// barrier releases. Command Tasks run under `tool_root`, the operator's tool
/// ceiling.
pub async fn run_trigger_engine(
    node: Arc<defra_node::Node>,
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    sources: Vec<Box<dyn TriggerSource>>,
    tool_root: Option<std::path::PathBuf>,
    cancel: CancellationToken,
) {
    let materializer: Arc<dyn MaterializerHandle> = Arc::new(
        production_materializer::ProductionMaterializer::new(node.clone(), snapshot_rx.clone()),
    );
    TriggerEngine::new(snapshot_rx, materializer)
        .with_command_host(Arc::new(command::ProductionCommandHost::new(
            node, tool_root,
        )))
        .run(sources, cancel)
        .await;
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

pub fn trigger_display_body(request: &gents_protocol::row::AgentRequestRow) -> Option<&str> {
    let gents_session::request::lifecycle::RequestOrigin::Trigger {
        trigger_id,
        trigger_kind,
    } = gents_session::request::lifecycle::request_origin(request)
    else {
        return None;
    };
    let kind = if trigger_kind == Some("event") {
        "event"
    } else {
        "scheduled"
    };
    request
        .content
        .as_deref()?
        .strip_prefix(&format!("[{kind} trigger:{trigger_id} · task:"))?
        .split_once("]\n\n")
        .map(|(_, body)| body)
}

pub struct FireIntent {
    pub trigger_id: Option<String>,
    pub trigger_kind: TriggerKind,
    pub task: gents_documents::automation::ResolvedTask,
    pub concurrency: gents_documents::ConcurrencyMode,
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

#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub(crate) struct MaterializeBudgetExhausted {
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
#[error("fire already admitted as {request_id}")]
pub(crate) struct MaterializeDuplicate {
    pub request_id: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MaterializeError {
    #[error(transparent)]
    BudgetExhausted(#[from] MaterializeBudgetExhausted),
    #[error(transparent)]
    Duplicate(#[from] MaterializeDuplicate),
    #[error(fmt = gents_protocol::error_chain::held)]
    Other(#[from] anyhow::Error),
}

pub(crate) fn fire_result_from_materialize(result: Result<String, MaterializeError>) -> FireResult {
    match result {
        Ok(request_id) => FireResult::Fired { request_id },
        Err(MaterializeError::BudgetExhausted(exhausted)) => FireResult::BudgetExhausted {
            reason: exhausted.reason,
        },
        Err(MaterializeError::Duplicate(duplicate)) => FireResult::Duplicate {
            request_id: duplicate.request_id,
        },
        Err(error) => FireResult::Errored {
            error: format!("materialize: {error}"),
        },
    }
}

pub(crate) const SERIAL_BUSY: &str = "serial: prior fire still in-flight";

pub const UNACKNOWLEDGED_STATUS: &str = "unacknowledged";

#[derive(Debug, Clone)]
pub enum FireResult {
    Duplicate {
        request_id: String,
    },
    Fired {
        request_id: String,
    },
    Skipped {
        reason: String,
    },
    Errored {
        error: String,
    },
    Rejected {
        error: String,
    },
    BudgetExhausted {
        reason: String,
    },
    /// A command Task's fire launched its command; no request was written.
    CommandStarted {
        run_id: String,
    },
}

pub trait TriggerSource: Send + Sync {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>;
}

/// One fire a materializer turns into a request: the Task and its rendered
/// prompt, the trigger and source that caused it, and the fire's durable
/// identity, prepared delivery and reserved request and session ids.
pub(crate) struct FireMaterialization<'a> {
    pub(crate) task: &'a gents_documents::automation::ResolvedTask,
    pub(crate) trigger_id: Option<&'a str>,
    pub(crate) trigger_kind: TriggerKind,
    pub(crate) trigger_doc_id: Option<&'a str>,
    pub(crate) source_doc_id: Option<&'a str>,
    pub(crate) correlation: Option<&'a str>,
    pub(crate) trigger_context: Option<&'a str>,
    pub(crate) rendered_prompt: &'a str,
    pub(crate) rendered_goal_objective: Option<&'a str>,
    pub(crate) durable_fire_key: &'a str,
    pub(crate) delivery: Option<&'a durable::PreparedFire>,
    pub(crate) prepared_ids: Option<(&'a str, &'a str)>,
}

pub(crate) trait MaterializerHandle: Send + Sync {
    fn materialize(
        &self,
        fire: FireMaterialization<'_>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, MaterializeError>> + Send + '_>,
    >;

    /// Recovers an admitted fire as the Agent of the fire's Task. The fire's
    /// receipt and request are that Agent's rows, so recovery does not depend
    /// on the trigger still being configured in the current snapshot.
    fn recover_admitted_fire(
        &self,
        _identity: &gents_protocol::trigger_delivery::FireIdentity,
        _task: &gents_documents::automation::ResolvedTask,
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
        task: &gents_documents::automation::ResolvedTask,
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
    command_host: Option<Arc<dyn command::CommandHost>>,
    command_runs: Arc<Mutex<command::CommandRuns>>,
    command_shutdown: CancellationToken,
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
            command_host: None,
            command_runs: Arc::default(),
            command_shutdown: CancellationToken::new(),
        }
    }

    pub(crate) fn with_command_host(mut self, host: Arc<dyn command::CommandHost>) -> Self {
        self.command_host = Some(host);
        self
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
        engine.command_shutdown.cancel();
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
            .materialize(FireMaterialization {
                task: &intent.task,
                trigger_id: intent.trigger_id.as_deref(),
                trigger_kind: intent.trigger_kind,
                trigger_doc_id: trigger_doc_id.as_deref(),
                source_doc_id: source_doc_id.as_deref(),
                correlation: intent.correlation.as_deref(),
                trigger_context: intent.trigger_context.as_deref(),
                rendered_prompt: &rendered,
                rendered_goal_objective: rendered_goal_objective.as_deref(),
                durable_fire_key: &intent.durable_fire_key,
                delivery: delivery.as_ref(),
                prepared_ids: prepared_ids
                    .as_ref()
                    .map(|(request, session)| (request.as_str(), session.as_str())),
            })
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
