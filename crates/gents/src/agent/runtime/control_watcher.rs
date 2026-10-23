use rapidhash::RapidHashMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, watch};

use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::runtime_status::{ReconcilePhase, RuntimeStatusHandle};

use super::super::document_view;
use super::super::DocumentResolveContext;

pub(super) const CONTROL_RECONCILE_DEBOUNCE: Duration = Duration::from_secs(5);
pub(super) const CONTROL_RECONCILE_LOCAL_DEBOUNCE: Duration = Duration::from_millis(250);
pub(super) const CONTROL_RECONCILE_SETTLE_RETRY: Duration = Duration::from_secs(1);
const CONTROL_RECONCILE_SETTLE_WINDOW: Duration = Duration::from_secs(60);
const CONTROL_WATCHER_IDLE_SLEEP: Duration = Duration::from_secs(60 * 60 * 24 * 365);

#[derive(Clone, Copy)]
pub(super) struct ControlWatcherTiming {
    pub(super) debounce: Duration,
    pub(super) local_debounce: Duration,
    pub(super) settle_retry: Duration,
    pub(super) settle_window: Duration,
    pub(super) idle_sleep: Duration,
}

const CONTROL_WATCHER_TIMING: ControlWatcherTiming = ControlWatcherTiming {
    debounce: CONTROL_RECONCILE_DEBOUNCE,
    local_debounce: CONTROL_RECONCILE_LOCAL_DEBOUNCE,
    settle_retry: CONTROL_RECONCILE_SETTLE_RETRY,
    settle_window: CONTROL_RECONCILE_SETTLE_WINDOW,
    idle_sleep: CONTROL_WATCHER_IDLE_SLEEP,
};

/// Coalesced changes to the control collections, with each subscribed
/// collection id mapped back to its name.
pub(super) struct ControlSubscription {
    changes: events::DocumentChangeSubscription,
    names_by_id: RapidHashMap<String, &'static str>,
}

pub(super) fn subscribe_control_changes(
    node: &defra_node::EmbeddedNode,
) -> Result<ControlSubscription> {
    let names = document_view::control_collection_names().collect::<Vec<_>>();
    let ids = crate::collection_ids(node, names.iter().copied())?;
    Ok(ControlSubscription {
        changes: node.subscribe_collection_changes(ids.clone()),
        names_by_id: ids.into_iter().zip(names).collect(),
    })
}

pub(super) async fn run_control_watcher(
    node: Arc<defra_node::EmbeddedNode>,
    subscription: ControlSubscription,
    node_did: String,
    resolve_context: DocumentResolveContext,
    proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    runtime_status: RuntimeStatusHandle,
    health_events_rx: mpsc::Receiver<()>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    run_control_watcher_with_timing(
        node,
        subscription,
        node_did,
        resolve_context,
        proposals_tx,
        runtime_status,
        health_events_rx,
        shutdown,
        CONTROL_WATCHER_TIMING,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_control_watcher_with_timing(
    node: Arc<defra_node::EmbeddedNode>,
    subscription: ControlSubscription,
    node_did: String,
    resolve_context: DocumentResolveContext,
    proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    runtime_status: RuntimeStatusHandle,
    mut health_events_rx: mpsc::Receiver<()>,
    mut shutdown: watch::Receiver<bool>,
    timing: ControlWatcherTiming,
) -> Result<()> {
    let mut document_view = document_view::load_document_runtime_view(&node, &node_did).await?;
    let sleep = tokio::time::sleep(timing.idle_sleep);
    tokio::pin!(sleep);
    let mut dirty = false;
    let mut pending_visibility = false;
    let mut settle_deadline = None;
    let mut last_proposed_fingerprint = None::<String>;
    let mut phase_transition_pending = false;
    let mut resync_required = false;
    let mut replicated_pending = false;
    let ControlSubscription {
        changes: mut subscription,
        names_by_id,
    } = subscription;
    let mut measured_mcp_availability =
        crate::tool_surface::measured_available_mcp_service_ids(&node, &node_did)
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = &mut sleep, if dirty => {
                replicated_pending = false;
                if resync_required || pending_visibility || settle_deadline.is_some() {
                    match document_view::load_document_runtime_view(&node, &node_did).await {
                        Ok(reloaded) => {
                            document_view = reloaded;
                            pending_visibility = document_view.has_unresolved_agent_references();
                            resync_required = false;
                        }
                        Err(error) => {
                            tracing::error!(
                                node_did = %node_did,
                                error = %error,
                                "runtime control watcher failed to refresh document view during settle window"
                            );
                            runtime_status.publish_error(&format!("{error:#}")).await;
                            if resync_required
                                || settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline)
                            {
                                dirty = true;
                                sleep.as_mut().reset(tokio::time::Instant::now() + timing.settle_retry);
                            } else {
                                dirty = false;
                                settle_deadline = None;
                                sleep.as_mut().reset(tokio::time::Instant::now() + timing.idle_sleep);
                            }
                            continue;
                        }
                    }
                }
                if pending_visibility
                    && settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
                {
                    let pending_details = document_view.pending_visibility_details();
                    let pending_summary = super::router::format_pending_visibility_error(&pending_details);
                    tracing::warn!(
                        node_did = %node_did,
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
                    sleep.as_mut().reset(tokio::time::Instant::now() + timing.settle_retry);
                    continue;
                }
                let phase_announced = phase_transition_pending;
                if phase_announced {
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Resolving)
                        .await;
                    phase_transition_pending = false;
                }
                let mut proposed_update = false;
                let mut resolve_failed = false;
                match document_view::resolve_document_runtime_snapshot_from_view(
                    &node,
                    &resolve_context,
                    &document_view,
                )
                .await
                {
                    Ok(snapshot) => {
                        let fingerprint = snapshot.configuration_fingerprint();
                        if phase_announced || last_proposed_fingerprint.as_deref() != Some(fingerprint.as_str()) {
                            if !phase_announced {
                                runtime_status
                                    .set_reconcile_phase(ReconcilePhase::Resolving)
                                    .await;
                            }
                            if proposals_tx.send(snapshot).await.is_err() {
                                return Ok(());
                            }
                            last_proposed_fingerprint = Some(fingerprint);
                            proposed_update = true;
                        }
                    }
                    Err(error) => {
                        resolve_failed = true;
                        tracing::error!(
                            node_did = %node_did,
                            error = %error,
                            "runtime reconcile resolve failed; keeping previous active generation"
                        );
                        runtime_status.publish_error(&format!("{error:#}")).await;
                    }
                }
                if resolve_failed {
                    if settle_deadline
                        .is_some_and(|deadline| tokio::time::Instant::now() < deadline)
                    {
                        dirty = true;
                        sleep
                            .as_mut()
                            .reset(tokio::time::Instant::now() + timing.settle_retry);
                    } else {
                        dirty = false;
                        settle_deadline = None;
                        sleep
                            .as_mut()
                            .reset(tokio::time::Instant::now() + timing.idle_sleep);
                    }
                    continue;
                }
                if !proposed_update {
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Idle)
                        .await;
                }
                dirty = false;
                settle_deadline = None;
                sleep.as_mut().reset(tokio::time::Instant::now() + timing.idle_sleep);
            }
            Some(()) = health_events_rx.recv() => {
                tracing::info!(
                    node_did = %node_did,
                    "backend measured-health transition detected; scheduling reconcile"
                );
                dirty = true;
                phase_transition_pending = true;
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Debouncing)
                    .await;
                sleep.as_mut().reset(tokio::time::Instant::now() + timing.debounce);
            }
            batch = subscription.recv() => {
                let Some(batch) = batch else {
                    return Ok(());
                };

                if batch.resync_required {
                    tracing::warn!(
                        node_did = %node_did,
                        updates = batch.updates,
                        "runtime control watcher document-change capacity exceeded; scheduling full reconcile"
                    );
                    resync_required = true;
                    dirty = true;
                    phase_transition_pending = true;
                    settle_deadline =
                        Some(tokio::time::Instant::now() + timing.settle_window);
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Debouncing)
                        .await;
                    sleep.as_mut().reset(tokio::time::Instant::now() + timing.debounce);
                    continue;
                }

                for update in batch.changes {
                    let collection_name = names_by_id
                        .get(&update.collection_id)
                        .copied()
                        .unwrap_or(update.collection_id.as_str());
                    if collection_name == "ToolServiceHealthState" {
                        let current = match crate::tool_surface::measured_available_mcp_service_ids(
                            &node,
                            &node_did,
                        )
                        .await
                        {
                            Ok(service_ids) => service_ids.into_iter().collect::<BTreeSet<_>>(),
                            Err(error) => {
                                tracing::warn!(
                                    node_did,
                                    %error,
                                    "could not measure MCP availability after health-state update"
                                );
                                continue;
                            }
                        };
                        if current == measured_mcp_availability {
                            continue;
                        }
                        measured_mcp_availability = current;
                    }
                    match document_view::apply_control_update(
                        &node_did,
                        collection_name,
                        &update.doc_id,
                        &mut document_view,
                    )
                    .await
                    {
                        Ok(document_view::ControlUpdateOutcome::Irrelevant) => continue,
                        Ok(document_view::ControlUpdateOutcome::FullReload) => {
                            match document_view::load_document_runtime_view(&node, &node_did).await {
                                Ok(reloaded) => {
                                    document_view = reloaded;
                                    pending_visibility = document_view.has_unresolved_agent_references();
                                }
                                Err(error) => {
                                    tracing::error!(
                                        node_did = %node_did,
                                        error = %error,
                                        "runtime control watcher failed to reload document view; keeping previous active generation"
                                    );
                                    runtime_status.publish_error(&format!("{error:#}")).await;
                                    resync_required = true;
                                    dirty = true;
                                    sleep.as_mut().reset(
                                        tokio::time::Instant::now() + timing.settle_retry,
                                    );
                                    break;
                                }
                            }
                        }
                        Err(error) => {
                            tracing::error!(
                                node_did = %node_did,
                                collection_id = %update.collection_id,
                                doc_id = %update.doc_id,
                                error = %error,
                                "runtime control update apply failed; forcing full resync"
                            );
                            match document_view::load_document_runtime_view(&node, &node_did).await {
                                Ok(reloaded) => {
                                    document_view = reloaded;
                                    pending_visibility = document_view.has_unresolved_agent_references();
                                }
                                Err(resync_error) => {
                                    tracing::error!(
                                        node_did = %node_did,
                                        error = %resync_error,
                                        "runtime control watcher failed to resync document view after update error"
                                    );
                                    runtime_status
                                        .publish_error(&format!("{resync_error:#}"))
                                        .await;
                                    resync_required = true;
                                    dirty = true;
                                    sleep.as_mut().reset(
                                        tokio::time::Instant::now() + timing.settle_retry,
                                    );
                                    break;
                                }
                            }
                        }
                    }

                    tracing::info!(
                        node_did = %node_did,
                        doc_id = %update.doc_id,
                        collection_id = %update.collection_id,
                        has_local_write = update.has_local_write,
                        "runtime control update detected"
                    );
                    dirty = true;
                    phase_transition_pending = true;
                    settle_deadline =
                        Some(tokio::time::Instant::now() + timing.settle_window);
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Debouncing)
                        .await;
                    replicated_pending |= !update.has_local_write;
                    let debounce = if replicated_pending {
                        timing.debounce
                    } else {
                        timing.local_debounce
                    };
                    sleep.as_mut().reset(tokio::time::Instant::now() + debounce);
                }
            }
        }
    }
}
