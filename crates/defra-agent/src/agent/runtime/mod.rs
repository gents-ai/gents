mod context;
mod startup;
mod router;
mod control_watcher;

pub(super) use context::StartupBarrier;
pub(super) use startup::run_agent;
pub(in crate::agent) use router::default_hostname;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use router::{
    resolve_behavior_for_request, run_router_generation_observer,
    wait_for_next_request_with_latest_snapshot,
};
#[cfg(test)]
use crate::watcher::Watcher;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_node::EventName;
use tokio::sync::{mpsc, watch};

use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};

const CONTROL_RECONCILE_DEBOUNCE: Duration = Duration::from_secs(5);
const CONTROL_RECONCILE_SETTLE_RETRY: Duration = Duration::from_secs(1);
// Replicated control docs can arrive before their referenced DAGs materialize.
// Keep polling past the initial debounce instead of immediately marking the behavior unavailable.
const CONTROL_RECONCILE_SETTLE_WINDOW: Duration = Duration::from_secs(60);
const CONTROL_WATCHER_IDLE_SLEEP: Duration = Duration::from_secs(60 * 60 * 24 * 365);

async fn run_control_watcher(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: String,
    resolve_context: super::DocumentResolveContext,
    proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    runtime_status: RuntimeStatusHandle,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut document_view =
        super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await?;
    let mut subscription = node.subscribe(&[EventName::Update]);
    let sleep = tokio::time::sleep(CONTROL_WATCHER_IDLE_SLEEP);
    tokio::pin!(sleep);
    let mut dirty = false;
    let mut pending_visibility = false;
    let mut settle_deadline = None;
    let mut last_proposed_fingerprint = None::<String>;

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = &mut sleep, if dirty => {
                if pending_visibility || settle_deadline.is_some() {
                    match super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await {
                        Ok(reloaded) => {
                            document_view = reloaded;
                            pending_visibility = document_view.has_unresolved_behavior_references();
                        }
                        Err(error) => {
                            tracing::error!(
                                agent_did = %agent_did,
                                error = %error,
                                "runtime control watcher failed to refresh document view during settle window"
                            );
                            runtime_status.publish_error(&format!("{error:#}")).await;
                            if settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline) {
                                dirty = true;
                                sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_RETRY);
                            } else {
                                dirty = false;
                                settle_deadline = None;
                                sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_WATCHER_IDLE_SLEEP);
                            }
                            continue;
                        }
                    }
                }
                if pending_visibility
                    && settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
                {
                    let pending_details = document_view.pending_visibility_details();
                    let pending_summary = router::format_pending_visibility_error(&pending_details);
                    tracing::warn!(
                        agent_did = %agent_did,
                        pending_references = %pending_details.join("; "),
                        "runtime control watcher is still waiting for referenced control documents"
                    );
                    runtime_status
                        .publish_error(&pending_summary)
                        .await;
                }
                if pending_visibility
                {
                    dirty = true;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_RETRY);
                    continue;
                }
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Resolving)
                    .await;
                let mut proposed_update = false;
                match super::document_view::resolve_document_runtime_snapshot_from_view(
                    node.as_ref(),
                    &resolve_context,
                    &document_view,
                )
                .await
                {
                    Ok(snapshot) => {
                        let fingerprint = snapshot.configuration_fingerprint();
                        if last_proposed_fingerprint.as_deref() != Some(fingerprint.as_str()) {
                            if proposals_tx.send(snapshot).await.is_err() {
                                return Ok(());
                            }
                            last_proposed_fingerprint = Some(fingerprint);
                            proposed_update = true;
                        }
                    }
                    Err(error) => {
                        tracing::error!(
                            agent_did = %agent_did,
                            error = %error,
                            "runtime reconcile resolve failed; keeping previous active generation"
                        );
                        runtime_status.publish_error(&format!("{error:#}")).await;
                    }
                }
                if !proposed_update {
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Idle)
                        .await;
                }
                if settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline) {
                    dirty = true;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_RETRY);
                } else {
                    dirty = false;
                    settle_deadline = None;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_WATCHER_IDLE_SLEEP);
                }
            }
            message = subscription.recv() => {
                let Some(message) = message else {
                    return Ok(());
                };

                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(
                        agent_did = %agent_did,
                        dropped = dropped,
                        "runtime control watcher dropped events, forcing full reconcile"
                    );
                    match super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await {
                        Ok(reloaded) => {
                            document_view = reloaded;
                            pending_visibility = document_view.has_unresolved_behavior_references();
                        }
                        Err(error) => {
                            tracing::error!(
                                agent_did = %agent_did,
                                error = %error,
                                "runtime control watcher failed to resync document view after dropped events"
                            );
                            runtime_status.publish_error(&format!("{error:#}")).await;
                            continue;
                        }
                    }
                    dirty = true;
                    settle_deadline =
                        Some(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_WINDOW);
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Debouncing)
                        .await;
                    sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_DEBOUNCE);
                    continue;
                }

                let Some(update) = message.as_update() else {
                    continue;
                };
                match super::document_view::apply_control_update(
                    node.as_ref(),
                    &agent_did,
                    update.collection_id.as_str(),
                    &update.doc_id,
                    &mut document_view,
                )
                .await
                {
                    Ok(super::document_view::ControlUpdateOutcome::Irrelevant) => continue,
                    Ok(super::document_view::ControlUpdateOutcome::Applied) => {}
                    Ok(super::document_view::ControlUpdateOutcome::PendingVisibility) => {
                        pending_visibility = true;
                    }
                    Err(error) => {
                        tracing::error!(
                            agent_did = %agent_did,
                            collection_id = %update.collection_id,
                            doc_id = %update.doc_id,
                            error = %error,
                            "runtime control update apply failed; forcing full resync"
                        );
                        match super::document_view::load_document_runtime_view(node.as_ref(), &agent_did).await {
                            Ok(reloaded) => {
                                document_view = reloaded;
                                pending_visibility = document_view.has_unresolved_behavior_references();
                            }
                            Err(resync_error) => {
                                tracing::error!(
                                    agent_did = %agent_did,
                                    error = %resync_error,
                                    "runtime control watcher failed to resync document view after update error"
                                );
                                runtime_status
                                    .publish_error(&format!("{resync_error:#}"))
                                    .await;
                                continue;
                            }
                        }
                    }
                }

                tracing::info!(
                    agent_did = %agent_did,
                    doc_id = %update.doc_id,
                    collection_id = %update.collection_id,
                    is_relay = update.is_relay,
                    "runtime control update detected"
                );
                dirty = true;
                settle_deadline =
                    Some(tokio::time::Instant::now() + CONTROL_RECONCILE_SETTLE_WINDOW);
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Debouncing)
                    .await;
                sleep.as_mut().reset(tokio::time::Instant::now() + CONTROL_RECONCILE_DEBOUNCE);
            }
        }
    }
}

#[cfg(test)]
mod tests;
