use rapidhash::{RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::Arc;

use defra_node::EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::UpdateSubscriptionSource;

mod claim;
mod documents;
mod planner;
pub(crate) mod plugin;
mod run;
mod scan;
mod workers;

#[cfg(test)]
mod tests;

pub use documents::reject_secret_bearing_callback_fields;
#[cfg(test)]
pub(crate) use documents::strip_secret_fields;
#[cfg(test)]
pub(crate) use documents::{
    create_callback_result, create_pending_invocation, update_invocation, CallbackInvocationDoc,
    CallbackResultDoc,
};
pub(crate) use documents::{idempotency_key, load_invocation};
pub(crate) use run::recover_local_invocations;

#[derive(Debug, thiserror::Error)]
pub enum CallbackFieldError {
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error("CallbackBinding {name} must be non-empty")]
    BindingFieldEmpty { name: &'static str },
    #[error("duplicate callback input field {field}")]
    DuplicateInputField { field: String },
    #[error("CallbackBinding {binding_id} source field `{field}` is secret-bearing")]
    SecretSourceField { binding_id: String, field: String },
    #[error("CallbackBinding {binding_id} filter field `{field}` is secret-bearing")]
    SecretFilterField { binding_id: String, field: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CallbackError {
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Response(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error(transparent)]
    DesiredState(Box<crate::config_client::DesiredStateError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Field(#[from] CallbackFieldError),
    #[error("ambiguous or foreign CallbackBinding")]
    AmbiguousOrForeignBinding,
    #[error("query CallbackInvocation {invocation_id}")]
    QueryInvocation {
        invocation_id: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("query CallbackInvocation states {states}")]
    QueryInvocationStates {
        states: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("query recent failed CallbackInvocation")]
    QueryRecentFailed(#[source] crate::config_client::ConfigClientError),
    #[error("query CallbackResult for {invocation_id}")]
    QueryResult {
        invocation_id: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("ambiguous node-scoped callback/workspace document")]
    AmbiguousNodeDocument,
    #[error("query IsolatedWorkspace {workspace_id}")]
    QueryIsolatedWorkspace {
        workspace_id: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("query WorkspacePlacement {workspace_id}")]
    QueryWorkspacePlacement {
        workspace_id: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("created CallbackInvocation missing after write")]
    InvocationMissingAfterWrite,
    #[error("create_CallbackInvocation")]
    CreateInvocation(#[source] crate::config_client::ConfigClientError),
    #[error("created CallbackResult missing after write")]
    ResultMissingAfterWrite,
    #[error("create_CallbackResult")]
    CreateResult(#[source] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Documents(#[from] crate::workspace::DocumentsError),
    #[error("persist IsolatedWorkspace {workspace_id}")]
    PersistIsolatedWorkspace {
        workspace_id: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error("persist WorkspacePlacement {workspace_id}")]
    PersistWorkspacePlacement {
        workspace_id: String,
        #[source]
        source: crate::config_client::ConfigClientError,
    },
    #[error(transparent)]
    Run(#[from] run::CallbackRunError),
    #[error("CallbackInvocation {invocation_id} could not persist claimed→running; state={state}")]
    PersistRunningRejected {
        invocation_id: String,
        state: String,
    },
    #[error("CallbackInvocation {invocation_id} disappeared during claimed→running")]
    DisappearedDuringRunning { invocation_id: String },
    #[error("ActionPlan has no actions")]
    EmptyActionPlan,
    #[error("CallbackInvocation {invocation_id} persist matched no row (state={state})")]
    PersistMatchedNoRow {
        invocation_id: String,
        state: String,
    },
    #[error("CallbackInvocation {invocation_id} deny persist matched no row")]
    DenyMatchedNoRow { invocation_id: String },
    #[error("CallbackInvocation {invocation_id} result docs are not ready")]
    ResultDocsNotReady { invocation_id: String },
    #[error("{reason}")]
    StoredActionPlan { reason: String },
    #[error(transparent)]
    EventDelivery(#[from] crate::trigger_engine::event_delivery::EventDeliveryError),
    #[error(transparent)]
    EventSource(Box<crate::trigger_engine::event_source::EventSourceError>),
    #[error(transparent)]
    Cursor(#[from] crate::config_client::event_source_cursor::EventSourceCursorError),
    #[error(transparent)]
    GraphPipeline(Box<crate::graph_pipeline::GraphPipelineError>),
    #[error("arrival query omitted data")]
    ArrivalOmittedData,
    #[error("arrival query omitted entries")]
    ArrivalOmittedEntries,
    #[error("arrival lacks document ID")]
    ArrivalMissingDocId,
    #[error("arrival lacks cursor")]
    ArrivalMissingCursor,
    #[error("arrival query omitted next cursor")]
    ArrivalOmittedNextCursor,
    #[error("CallbackBinding {binding_id} EventSource missing for owner")]
    EventSourceMissing { binding_id: String },
    #[error("callback EventSource requires supported created event kind")]
    UnsupportedEventKind,
    #[error("Callback {callback_id} missing for owner")]
    CallbackMissingForOwner { callback_id: String },
    #[error("callback group correlation missing")]
    GroupCorrelationMissing,
    #[error("callback missing for owner")]
    CallbackMissing,
    #[error("callback input field is not a scalar source field")]
    GroupFieldNotScalar,
    #[error("callback input field {field} is not a safe scalar source field")]
    UnsafeSourceField { field: String },
    #[error("source doc {source_doc_id} not found in {collection}")]
    SourceDocNotFound {
        source_doc_id: String,
        collection: String,
    },
    #[error("callback source {source_doc_id} has no composite version")]
    SourceMissingVersion { source_doc_id: String },
    #[error(
        "callback source {source_doc_id} changed after invocation: expected version {expected}, found {found}"
    )]
    SourceChanged {
        source_doc_id: String,
        expected: String,
        found: String,
    },
    #[error("callback source is not an object")]
    SourceNotObject,
}

impl From<crate::trigger_engine::event_source::EventSourceError> for CallbackError {
    fn from(error: crate::trigger_engine::event_source::EventSourceError) -> Self {
        Self::EventSource(Box::new(error))
    }
}

impl From<crate::config_client::DesiredStateError> for CallbackError {
    fn from(error: crate::config_client::DesiredStateError) -> Self {
        Self::DesiredState(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for CallbackError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Response(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::DesiredState(error) => {
                crate::config_client::TransactionFailure::config_client_error(error.as_ref())
            }
            Self::EventDelivery(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::EventSource(error) => {
                crate::config_client::TransactionFailure::config_client_error(error.as_ref())
            }
            Self::Cursor(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::GraphPipeline(error) => {
                crate::config_client::TransactionFailure::config_client_error(error.as_ref())
            }
            Self::CreateInvocation(error)
            | Self::CreateResult(error)
            | Self::PersistIsolatedWorkspace { source: error, .. }
            | Self::PersistWorkspacePlacement { source: error, .. } => Some(error),
            _ => None,
        }
    }
}

pub(crate) const LIFECYCLE_PENDING: &str = "pending";
pub(crate) const LIFECYCLE_CLAIMED: &str = "claimed";
pub(crate) const LIFECYCLE_RUNNING: &str = "running";
pub(crate) const LIFECYCLE_SUCCEEDED: &str = "succeeded";
pub(crate) const LIFECYCLE_FAILED: &str = "failed";
pub(crate) const LIFECYCLE_DENIED: &str = "denied";

pub(super) struct CallbackEngine {
    node: Arc<EmbeddedNode>,
    node_did: String,
    ceiling: Option<PathBuf>,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
    subscription: Option<events::Subscription>,
    desired_collections: RapidHashSet<String>,
    seen_docs: RapidHashMap<String, RapidHashSet<String>>,
    cursor_bindings: RapidHashMap<String, String>,
    group_collections: RapidHashSet<String>,
    collection_id_to_name: RapidHashMap<String, String>,
    group_page_cursors: RapidHashMap<String, String>,
    group_recovery_cursor: usize,
    rescan_tick: tokio::time::Interval,
    cancel: CancellationToken,
    workers: Option<workers::CallbackWorkers>,
}

pub async fn run_callback_engine(
    node: Arc<EmbeddedNode>,
    node_did: String,
    ceiling: Option<PathBuf>,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    cancel: CancellationToken,
) -> Result<(), CallbackError> {
    let mut engine = CallbackEngine::new(node, node_did.clone(), ceiling.clone(), cancel.clone());
    engine.plugins = plugins;
    engine.workers = Some(workers::CallbackWorkers::new(
        workers::PLUGIN_CALLBACK_CONCURRENCY,
        cancel,
    ));
    engine.reconcile_bindings().await;
    if let Err(error) = recover_local_invocations(
        &engine.node,
        &node_did,
        ceiling.as_deref(),
        &engine.plugins,
        Some(&engine),
    )
    .await
    {
        tracing::warn!(%error, "callback recovery sweep failed at startup");
    }

    let result = loop {
        if engine.cancel.is_cancelled() {
            break Ok(());
        }
        if engine.subscription.is_none() && !engine.desired_collections.is_empty() {
            engine.subscription = Some(engine.subscription_source.subscribe_updates());
        }

        if engine.subscription.is_none() {
            tokio::select! {
                biased;
                _ = engine.cancel.cancelled() => break Ok(()),
                _ = engine.rescan_tick.tick() => {
                    engine.reconcile_bindings().await;
                    engine.rescan_created_docs().await;
                    if let Err(error) = recover_local_invocations(
                        &engine.node,
                        &engine.node_did,
                        engine.ceiling.as_deref(),
                        &engine.plugins,
                        Some(&engine),
                    )
                    .await
                    {
                        tracing::warn!(%error, "callback recovery sweep failed");
                    }
                }
            }
            continue;
        }

        let mut message = None;
        let rescan_due = {
            let subscription = engine
                .subscription
                .as_mut()
                .expect("subscription is Some when desired_collections is non-empty");
            tokio::select! {
                biased;
                _ = engine.cancel.cancelled() => break Ok(()),
                _ = engine.rescan_tick.tick() => true,
                msg = subscription.recv() => {
                    message = msg;
                    false
                }
            }
        };
        if rescan_due {
            engine.reconcile_bindings().await;
            engine.rescan_created_docs().await;
            if let Err(error) = recover_local_invocations(
                &engine.node,
                &engine.node_did,
                engine.ceiling.as_deref(),
                &engine.plugins,
                Some(&engine),
            )
            .await
            {
                tracing::warn!(%error, "callback recovery sweep failed");
            }
            continue;
        }
        let Some(message) = message else {
            break Ok(());
        };
        let Some(update) = message.as_update() else {
            continue;
        };
        engine
            .handle_update(&update.collection_id, &update.doc_id)
            .await;
    };
    engine
        .workers
        .as_ref()
        .expect("live callback workers")
        .drain()
        .await;
    result
}

#[cfg(test)]
pub(crate) async fn scan_callbacks(
    node: Arc<EmbeddedNode>,
    node_did: String,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
) {
    let mut engine = CallbackEngine::new(node, node_did, None, CancellationToken::new());
    engine.plugins = plugins;
    engine.reconcile_bindings().await;
    engine.rescan_created_docs().await;
}
