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
    create_callback_result, create_pending_invocation, CallbackInvocationDoc, CallbackResultDoc,
};
pub(crate) use documents::{idempotency_key, load_invocation};
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

pub async fn run_callback_engine(
    node: Arc<EmbeddedNode>,
    node_did: String,
    ceiling: Option<PathBuf>,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    cancel: CancellationToken,
) -> Result<()> {
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
pub(crate) fn test_agent_did() -> String {
    static DID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DID.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        let root = crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path());
        crate::store::identity::derive_agent_identity(&root, &"c".repeat(64))
            .unwrap()
            .did()
            .to_owned()
    })
    .clone()
}

#[cfg(test)]
pub(crate) fn test_agent_access(
    node: &std::sync::Arc<defra_node::Node>,
) -> crate::store::config_client::ConfigAccess {
    crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(test_agent_did()).unwrap(),
    )
}

#[cfg(test)]
pub(crate) async fn seed_test_agent(node: &std::sync::Arc<defra_node::Node>, owner: &str) {
    use crate::store::identity::NodeIdentity as _;
    let home = tempfile::tempdir().unwrap();
    let user = crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path());
    let agent = test_agent_did();
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(user.did()).unwrap(),
    );
    let (node_doc, agent_doc) = access.transact_write("test.callback.seed_agent", |txn| {
        let agent = &agent;
        Box::pin(async move {
            let owner = crate::store::graphql::escape_graphql_string(owner);
            let created = txn.execute(&format!(
                "mutation {{ create_Node(input: {{node_did: \"{owner}\", display_name: \"test\", enabled: true}}) {{ _docID }} }}"
            )).await?;
            let node_doc = crate::store::graphql::created_doc_id(&created, "Node")?;
            let created = txn.execute(&format!(
                "mutation {{ create_Agent(input: {{node_did: \"{owner}\", agent_did: \"{}\", creation_nonce: \"{}\", enabled: true}}) {{ _docID }} }}",
                crate::store::graphql::escape_graphql_string(agent), "c".repeat(64),
            )).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
            Ok::<_, anyhow::Error>((node_doc, agent_doc))
        })
    }).await.unwrap();
    access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(&agent).unwrap(),
        )
        .await
        .unwrap();
    if let Some(runtime) = node.node_identity_did() {
        access
            .add_actor_relationship(
                "Node",
                &node_doc,
                "runtime_reader",
                &identity::Did::new(runtime).unwrap(),
            )
            .await
            .unwrap();
    }
}
