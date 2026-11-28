use rapidhash::{RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use tokio_util::sync::CancellationToken;

use gents_store::subscription_source::UpdateSubscriptionSource;

mod claim;
mod documents;
mod planner;
pub(crate) mod plugin;
mod run;
mod scan;
mod workers;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use documents::{
    create_callback_result, create_pending_invocation, update_invocation, CallbackInvocationDoc,
    CallbackResultDoc,
};
pub(crate) use run::recover_local_invocations;

pub(crate) const LIFECYCLE_PENDING: &str = "pending";
pub(crate) const LIFECYCLE_CLAIMED: &str = "claimed";
pub(crate) const LIFECYCLE_RUNNING: &str = "running";
pub(crate) const LIFECYCLE_SUCCEEDED: &str = "succeeded";
pub(crate) const LIFECYCLE_FAILED: &str = "failed";
pub(crate) const LIFECYCLE_DENIED: &str = "denied";

pub(crate) struct CallbackEngine {
    node: Arc<EmbeddedNode>,
    node_did: String,
    snapshots: Option<
        tokio::sync::watch::Receiver<Arc<crate::triggers::runtime_snapshot::ActiveRuntimeSnapshot>>,
    >,
    ceiling: Option<PathBuf>,
    plugins: Arc<gents_toolcalls::plugin::executor::PluginExecutor>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
    subscription: Option<events::Subscription>,
    lifecycle_subscription: Option<gents_store::subscription_source::ChangeSubscription>,
    config_collection_ids: RapidHashSet<String>,
    desired_collections: RapidHashSet<String>,
    active_binding_agents: RapidHashSet<String>,
    seen_docs: RapidHashMap<String, RapidHashMap<String, RapidHashSet<String>>>,
    cursor_bindings: RapidHashMap<String, String>,
    group_collections: RapidHashSet<String>,
    group_actors: RapidHashMap<String, RapidHashSet<String>>,
    collection_id_to_name: RapidHashMap<String, String>,
    group_page_cursors: RapidHashMap<String, String>,
    group_recovery_cursor: usize,
    non_retryable_failed: RapidHashSet<(String, i64, String)>,
    recovery_has_open_work: bool,
    next_retry_at: Option<chrono::DateTime<chrono::Utc>>,
    rescan_tick: tokio::time::Interval,
    cancel: CancellationToken,
    workers: Option<workers::CallbackWorkers>,
}

async fn run_recovery_sweep(engine: &mut CallbackEngine, context: &str) {
    let node = engine.node.clone();
    let node_did = engine.node_did.clone();
    let ceiling = engine.ceiling.clone();
    let plugins = engine.plugins.clone();
    match recover_local_invocations(&node, &node_did, ceiling.as_deref(), &plugins, engine).await {
        Ok(outcome) => {
            engine.recovery_has_open_work = outcome.has_open_work;
            engine.next_retry_at = outcome.next_retry_at;
        }
        Err(error) => {
            engine.recovery_has_open_work = true;
            tracing::warn!(%error, "{context}");
        }
    }
}

pub async fn run_callback_engine(
    node: Arc<EmbeddedNode>,
    node_did: String,
    ceiling: Option<PathBuf>,
    plugins: Arc<gents_toolcalls::plugin::executor::PluginExecutor>,
    snapshots: tokio::sync::watch::Receiver<
        Arc<crate::triggers::runtime_snapshot::ActiveRuntimeSnapshot>,
    >,
    cancel: CancellationToken,
) -> Result<()> {
    let mut engine = CallbackEngine::new(node, node_did.clone(), ceiling.clone(), cancel.clone());
    engine.snapshots = Some(snapshots);
    engine.plugins = plugins;
    engine.workers = Some(workers::CallbackWorkers::new(
        workers::PLUGIN_CALLBACK_CONCURRENCY,
        cancel,
    ));
    engine.ensure_lifecycle_subscription();
    engine.reconcile_bindings().await;
    run_recovery_sweep(&mut engine, "callback recovery sweep failed at startup").await;
    engine.rescan_tick.reset();

    enum EngineWake {
        Cancelled,
        SnapshotChanged,
        SnapshotClosed,
        Lifecycle(Option<events::DocumentChangeBatch>),
        RetryDue,
        FallbackTick,
        SourceUpdate(Option<events::Message>),
    }

    let mut snapshots_open = true;

    let result = loop {
        if engine.cancel.is_cancelled() {
            break Ok(());
        }
        engine.ensure_lifecycle_subscription();
        if engine.subscription.is_none() && !engine.desired_collections.is_empty() {
            engine.subscription = Some(engine.subscription_source.subscribe_updates());
        }

        let retry_delay = engine
            .next_retry_at
            .map(|at| (at - chrono::Utc::now()).to_std().unwrap_or_default());

        let wake = {
            let cancel = &engine.cancel;
            let snapshots = &mut engine.snapshots;
            let lifecycle = &mut engine.lifecycle_subscription;
            let source_sub = &mut engine.subscription;
            let rescan_tick = &mut engine.rescan_tick;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => EngineWake::Cancelled,
                changed = async {
                    match snapshots.as_mut() {
                        Some(rx) => rx.changed().await,
                        None => std::future::pending().await,
                    }
                }, if snapshots_open => {
                    if changed.is_err() {
                        EngineWake::SnapshotClosed
                    } else {
                        EngineWake::SnapshotChanged
                    }
                }
                batch = async {
                    match lifecycle.as_mut() {
                        Some(sub) => sub.recv().await,
                        None => std::future::pending().await,
                    }
                } => EngineWake::Lifecycle(batch),
                _ = tokio::time::sleep(retry_delay.unwrap_or_default()), if retry_delay.is_some() => {
                    EngineWake::RetryDue
                }
                _ = rescan_tick.tick() => EngineWake::FallbackTick,
                msg = async {
                    match source_sub.as_mut() {
                        Some(sub) => sub.recv().await,
                        None => std::future::pending().await,
                    }
                } => EngineWake::SourceUpdate(msg),
            }
        };

        match wake {
            EngineWake::Cancelled => break Ok(()),
            EngineWake::SnapshotClosed => {
                snapshots_open = false;
            }
            EngineWake::SnapshotChanged => {
                engine.clear_non_retryable_failed();
                engine.reconcile_bindings().await;
                if !engine.desired_collections.is_empty() {
                    engine.rescan_created_docs().await;
                }
                run_recovery_sweep(&mut engine, "callback recovery sweep failed").await;
            }
            EngineWake::Lifecycle(Some(batch)) => {
                let config_changed = batch.resync_required
                    || batch
                        .changes
                        .iter()
                        .any(|change| engine.config_collection_ids.contains(&change.collection_id));
                if config_changed {
                    engine.clear_non_retryable_failed();
                    engine.reconcile_bindings().await;
                    if !engine.desired_collections.is_empty() {
                        engine.rescan_created_docs().await;
                    }
                }
                run_recovery_sweep(&mut engine, "callback recovery sweep failed").await;
            }
            EngineWake::Lifecycle(None) => {
                engine.lifecycle_subscription = None;
                engine.recovery_has_open_work = true;
                tracing::warn!(
                    "callback lifecycle subscription closed; continuing interval rescans"
                );
            }
            EngineWake::RetryDue => {
                engine.next_retry_at = None;
                run_recovery_sweep(&mut engine, "callback retry sweep failed").await;
            }
            EngineWake::FallbackTick => {
                let idle_with_live_subscription = engine.lifecycle_subscription.is_some()
                    && !engine.recovery_has_open_work
                    && engine.desired_collections.is_empty()
                    && engine.active_binding_agents.is_empty();
                if idle_with_live_subscription {
                    continue;
                }
                engine.reconcile_bindings().await;
                if !engine.desired_collections.is_empty() {
                    engine.rescan_created_docs().await;
                }
                run_recovery_sweep(&mut engine, "callback recovery sweep failed").await;
            }
            EngineWake::SourceUpdate(None) => break Ok(()),
            EngineWake::SourceUpdate(Some(message)) => {
                let Some(update) = message.as_update() else {
                    continue;
                };
                engine
                    .handle_update(&update.collection_id, &update.doc_id)
                    .await;
            }
        }
    };
    engine
        .workers
        .as_ref()
        .expect("live callback workers")
        .drain()
        .await;
    result
}

/// Starts a callback test node through the protected start as
/// [`gents_store::test_support::fixture_root`].
#[cfg(any(test, feature = "test-fixtures"))]
pub async fn test_node_builder(name: &str) -> gents_store::test_support::TestStore {
    use gents_store::identity::NodeIdentity as _;
    gents_store::test_support::TestStore::start_as(
        name,
        gents_store::test_support::fixture_root().did(),
        crate::lifecycle_hooks::composition(),
    )
    .await
}

#[cfg(test)]
pub(crate) fn test_snapshot(
) -> tokio::sync::watch::Receiver<Arc<crate::triggers::runtime_snapshot::ActiveRuntimeSnapshot>> {
    let agent =
        gents_store::test_support::fixture_agent(gents_store::test_support::FIXTURE_AGENT_NONCE);
    let mut unavailable_agents = rapidhash::RapidHashMap::default();
    unavailable_agents.insert(agent.did().to_owned(), crate::triggers::runtime_snapshot::UnavailableAgent::new(
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
        "callback test fixture",
    ).with_watcher_identity(agent.clone()));
    let snapshot = Arc::new(crate::triggers::runtime_snapshot::ActiveRuntimeSnapshot {
        placement_basis: Default::default(),
        placement_bodies: Default::default(),
        generation: 1,
        node: None,
        local_did: gents_store::test_support::PIN_FIXED_DID.into(),
        default_agent_did: agent.did().to_owned(),
        agents: Default::default(),
        tool_surfaces: Default::default(),
        tool_configs: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_agents,
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: Default::default(),
        agent_executor_capacities: Default::default(),
        agent_executor_queue_capacities: Default::default(),
    });
    tokio::sync::watch::channel(snapshot).1
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn seed_test_agent(node: &std::sync::Arc<defra_node::Node>, owner: &str) {
    use gents_store::test_support::{fixture_agent_did, FIXTURE_AGENT_NONCE};
    let agent = fixture_agent_did(FIXTURE_AGENT_NONCE);
    let local = node
        .node_identity_did()
        .expect("test node runtime identity");
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(local).unwrap(),
    );
    gents_store::identity::GrantSet::AgentBaseline
        .grant(&access, &identity::Did::new(&agent).unwrap())
        .await
        .unwrap();
    access.transact_write("test.callback.seed_agent", |txn| {
        let agent = &agent;
        let runtime = node.node_identity_did();
        Box::pin(async move {
            let owner = gents_store::graphql::escape_graphql_string(owner);
            let created = txn.execute(&format!(
                "mutation {{ create_Node(input: {{node_did: \"{owner}\", display_name: \"test\", enabled: true}}) {{ _docID }} }}"
            )).await?;
            let node_doc = gents_store::graphql::created_doc_id(&created, "Node")?;
            let created = txn.execute(&format!(
                "mutation {{ create_Agent(input: {{node_did: \"{owner}\", agent_did: \"{}\", creation_nonce: \"{}\", inference_profile_id: \"fixture-profile\", enabled: true}}) {{ _docID }} }}",
                gents_store::graphql::escape_graphql_string(agent), FIXTURE_AGENT_NONCE,
            )).await?;
            let agent_doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
            txn.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc).await?;
            txn.add_actor_relationship("Agent", &agent_doc, "executor", &identity::Did::new(agent)?).await?;
            if let Some(runtime) = runtime {
                txn.add_actor_relationship("Node", &node_doc, "runtime_reader", &identity::Did::new(runtime)?).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}

#[cfg(test)]
pub(crate) async fn seed_test_configuration(
    node: &Arc<defra_node::Node>,
    owner: &str,
    documents: Vec<(gents_store::collection::Collection, serde_json::Value)>,
) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let documents = documents
        .into_iter()
        .map(|(collection, mut value)| {
            value["node_did"] = serde_json::json!(owner);
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(documents).unwrap();
    let local = node
        .node_identity_did()
        .expect("test node runtime identity");
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(local).unwrap(),
    );
    access
        .transact_write("test.callback.seed_configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
}

#[cfg(test)]
pub(crate) async fn grant_test_node_administrator(node: &Arc<defra_node::Node>, user: &str) {
    let local = node
        .node_identity_did()
        .expect("test node runtime identity");
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(local).unwrap(),
    );
    for permission in [
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
    ] {
        access
            .grant_node_permission(&identity::Did::new(user).unwrap(), permission)
            .await
            .unwrap();
    }
    let rows = access.execute("{ Node { _docID } }").await.unwrap();
    let doc = rows["data"]["Node"][0]["_docID"].as_str().unwrap();
    access
        .add_actor_relationship(
            "Node",
            doc,
            "administrator",
            &identity::Did::new(user).unwrap(),
        )
        .await
        .unwrap();
}

#[cfg(test)]
pub(crate) async fn grant_test_node_read(node: &Arc<defra_node::Node>, user: &str) {
    let local = node
        .node_identity_did()
        .expect("test node runtime identity");
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(local).unwrap(),
    );
    access
        .grant_node_permission(
            &identity::Did::new(user).unwrap(),
            gents_store::config_client::NodePermission::DocumentRead,
        )
        .await
        .unwrap();
}
