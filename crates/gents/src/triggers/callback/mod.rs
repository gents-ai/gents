use rapidhash::{RapidHashMap, RapidHashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
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
