//! Callback engine: first-seen source creates → journaled host actions.
//!
//! Bindings select shared EventSource configuration (first-seen create semantics).
//! Invocations are claimable only on `owner_agent_did`. `CallbackResult`
//! is created only after IsolatedWorkspace + WorkspacePlacement are durable.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::UpdateSubscriptionSource;

mod claim;
mod documents;
mod run;
mod scan;
mod wasm;

#[cfg(test)]
mod tests;

pub use documents::reject_secret_bearing_callback_fields;
pub(crate) use documents::{
    flush_workspace_docs, load_isolated_workspace, load_repository_placement,
    load_workspace_placement,
};
pub(crate) use run::recover_local_invocations;

pub(crate) const LIFECYCLE_PENDING: &str = "pending";
pub(crate) const LIFECYCLE_CLAIMED: &str = "claimed";
pub(crate) const LIFECYCLE_RUNNING: &str = "running";
pub(crate) const LIFECYCLE_SUCCEEDED: &str = "succeeded";
pub(crate) const LIFECYCLE_FAILED: &str = "failed";
pub(crate) const LIFECYCLE_DENIED: &str = "denied";

pub(super) struct CallbackEngine {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    ceiling: Option<PathBuf>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
    subscription: Option<events::Subscription>,
    desired_collections: HashSet<String>,
    seen_docs: HashMap<String, HashSet<String>>,
    collection_id_to_name: HashMap<String, String>,
    group_page_cursors: HashMap<String, String>,
    group_recovery_cursor: usize,
    rescan_tick: tokio::time::Interval,
    cancel: CancellationToken,
}

pub async fn run_callback_engine(
    node: Arc<EmbeddedNode>,
    agent_did: String,
    ceiling: Option<PathBuf>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut engine = CallbackEngine::new(node, agent_did.clone(), ceiling.clone(), cancel.clone());
    engine.reconcile_bindings().await;
    if let Err(error) =
        recover_local_invocations(engine.node.as_ref(), &agent_did, ceiling.as_deref()).await
    {
        tracing::warn!(%error, "callback recovery sweep failed at startup");
    }

    loop {
        if engine.cancel.is_cancelled() {
            return Ok(());
        }
        if engine.subscription.is_none() && !engine.desired_collections.is_empty() {
            engine.subscription = Some(engine.subscription_source.subscribe_updates());
        }

        if engine.subscription.is_none() {
            tokio::select! {
                biased;
                _ = engine.cancel.cancelled() => return Ok(()),
                _ = engine.rescan_tick.tick() => {
                    engine.reconcile_bindings().await;
                    engine.rescan_created_docs().await;
                    if let Err(error) = recover_local_invocations(
                        engine.node.as_ref(),
                        &engine.agent_did,
                        engine.ceiling.as_deref(),
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
                _ = engine.cancel.cancelled() => return Ok(()),
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
                engine.node.as_ref(),
                &engine.agent_did,
                engine.ceiling.as_deref(),
            )
            .await
            {
                tracing::warn!(%error, "callback recovery sweep failed");
            }
            continue;
        }
        let Some(message) = message else {
            return Ok(());
        };
        let Some(update) = message.as_update() else {
            continue;
        };
        engine
            .handle_update(&update.collection_id, &update.doc_id)
            .await;
    }
}
