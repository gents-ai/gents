use rapidhash::{RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use defra_node::Node as EmbeddedNode;
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
    create_callback_result, create_pending_invocation, flush_workspace_docs, update_invocation,
    CallbackInvocationDoc, CallbackResultDoc,
};
#[cfg(test)]
pub(crate) use documents::load_invocation;
pub(crate) use documents::{idempotency_key, load_invocation_with_access};
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
    snapshots:
        Option<tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>>>,
    ceiling: Option<PathBuf>,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
    subscription: Option<events::Subscription>,
    desired_collections: RapidHashSet<String>,
    seen_docs: RapidHashMap<String, RapidHashMap<String, RapidHashSet<String>>>,
    cursor_bindings: RapidHashMap<String, String>,
    group_collections: RapidHashSet<String>,
    group_actors: RapidHashMap<String, RapidHashSet<String>>,
    collection_id_to_name: RapidHashMap<String, String>,
    group_page_cursors: RapidHashMap<String, String>,
    group_recovery_cursor: usize,
    rescan_tick: tokio::time::Interval,
    cancel: CancellationToken,
    workers: Option<workers::CallbackWorkers>,
}

pub(crate) async fn run_callback_engine(
    node: Arc<EmbeddedNode>,
    node_did: String,
    ceiling: Option<PathBuf>,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    snapshots: tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut engine = CallbackEngine::new(node, node_did.clone(), ceiling.clone(), cancel.clone());
    engine.snapshots = Some(snapshots);
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
        &engine,
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
                        &engine,
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
                &engine,
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
pub(crate) fn test_root_identity() -> crate::store::identity::KeyIdentity {
    let home = tempfile::tempdir().unwrap();
    crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path())
}

#[cfg(test)]
pub(crate) fn test_node_builder() -> defra_node::NodeBuilder {
    use crate::store::identity::NodeIdentity as _;
    defra_node::Node::builder()
        .with_node_identity_did(test_root_identity().did())
        .with_node_acp_enabled()
}

#[cfg(test)]
pub(crate) fn test_agent_identity() -> Arc<dyn crate::store::identity::NodeIdentity> {
    let root = test_root_identity();
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        creation_nonce: "c".repeat(64),
        agent_did: crate::store::identity::derive_agent_identity(&root, &"c".repeat(64))
            .unwrap()
            .did()
            .to_owned(),
    };
    crate::store::identity::resolve_agent_identity(&root, &binding).unwrap()
}

#[cfg(test)]
pub(crate) fn test_agent_did() -> String {
    test_agent_identity().did().to_owned()
}

#[cfg(test)]
pub(crate) fn test_agent_access(node: &Arc<defra_node::Node>) -> crate::ConfigAccess {
    let agent = test_agent_identity();
    crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}

#[cfg(test)]
pub(crate) fn test_snapshot(
) -> tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>> {
    let agent = test_agent_identity();
    let mut unavailable_agents = rapidhash::RapidHashMap::default();
    unavailable_agents.insert(agent.did().to_owned(), crate::runtime::snapshot::UnavailableAgent::new(
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
        "callback test fixture",
    ).with_watcher_identity(agent.clone()));
    let snapshot = Arc::new(crate::runtime::snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: crate::request::lifecycle::test_support::PIN_FIXED_DID.into(),
        default_agent_did: agent.did().to_owned(),
        agents: Default::default(),
        tool_surfaces: Default::default(),
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

#[cfg(test)]
pub(crate) async fn seed_test_agent(node: &std::sync::Arc<defra_node::Node>, owner: &str) {
    let agent = test_agent_did();
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(crate::request::lifecycle::test_support::PIN_FIXED_DID).unwrap(),
    );
    for permission in [
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
        crate::store::config_client::NodePermission::DacRelationAdd,
        crate::store::config_client::NodePermission::DacRelationDelete,
    ] {
        access
            .grant_node_permission(&identity::Did::new(&agent).unwrap(), permission)
            .await
            .unwrap();
    }
    access.transact_write("test.callback.seed_agent", |txn| {
        let agent = &agent;
        let runtime = node.node_identity_did();
        Box::pin(async move {
            let owner = crate::store::graphql::escape_graphql_string(owner);
            let created = txn.execute(&format!(
                "mutation {{ create_Node(input: {{node_did: \"{owner}\", display_name: \"test\", enabled: true}}) {{ _docID }} }}"
            )).await?;
            let node_doc = crate::store::graphql::created_doc_id(&created, "Node")?;
            let created = txn.execute(&format!(
                "mutation {{ create_Agent(input: {{node_did: \"{owner}\", agent_did: \"{}\", creation_nonce: \"{}\", inference_profile_id: \"fixture-profile\", enabled: true}}) {{ _docID }} }}",
                crate::store::graphql::escape_graphql_string(agent), "c".repeat(64),
            )).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
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
    documents: Vec<(crate::Collection, serde_json::Value)>,
) {
    use crate::store::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
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
    let access = crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(crate::request::lifecycle::test_support::PIN_FIXED_DID).unwrap(),
    );
    access
        .transact_write("test.callback.seed_configuration", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

#[cfg(test)]
pub(crate) async fn grant_test_node_administrator(node: &Arc<defra_node::Node>, user: &str) {
    let access = crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(crate::request::lifecycle::test_support::PIN_FIXED_DID).unwrap(),
    );
    for permission in [
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
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
    let access = crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(crate::request::lifecycle::test_support::PIN_FIXED_DID).unwrap(),
    );
    access
        .grant_node_permission(
            &identity::Did::new(user).unwrap(),
            crate::store::config_client::NodePermission::DocumentRead,
        )
        .await
        .unwrap();
}
