use rapidhash::RapidHashMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, watch};

use crate::runtime::status::RuntimeStatusHandle;
use gents_automation::triggers::runtime_snapshot::ResolvedRuntimeSnapshot;
use gents_protocol::node_readiness::ReconcilePhase;
use gents_tools::surface::ToolRuntimeContext;

use super::super::document_view;
use super::super::DocumentResolveContext;
use super::surface_publication::{
    epoch_deadline, reconcile, retry_after_failure, send_proposal, watch_store, Disposition,
    StoreWatch,
};
use gents_pack::plugin::store::{PublishedPluginReadError, ReadOutcome, StoreReadPin};
use gents_protocol::error_chain::ErrorChain;
use gents_store::config_client::placement_fence::PlacementBasis;

pub(in crate::runtime::agent) const CONTROL_RECONCILE_DEBOUNCE: Duration = Duration::from_secs(5);
pub(super) const CONTROL_RECONCILE_LOCAL_DEBOUNCE: Duration = Duration::from_millis(250);
pub(super) const CONTROL_RECONCILE_SETTLE_RETRY: Duration = Duration::from_secs(1);
const CONTROL_RECONCILE_SETTLE_WINDOW: Duration = Duration::from_secs(60);
const CONTROL_WATCHER_IDLE_SLEEP: Duration = Duration::from_secs(60 * 60 * 24 * 365);

#[derive(Clone, Copy)]
pub(super) struct ControlWatcherTiming {
    /// How long contention for the plugin store may hold back a reconciled
    /// configuration (Lean `SurfacePublication.epochDeadline`).
    pub(super) publication_wait: Duration,
    /// Also the longest a pending change waits for its reconcile after its
    /// first wake (`ControlDebounce.due`).
    pub(super) debounce: Duration,
    pub(super) local_debounce: Duration,
    pub(super) settle_retry: Duration,
    pub(super) settle_window: Duration,
    pub(super) idle_sleep: Duration,
}

const CONTROL_WATCHER_TIMING: ControlWatcherTiming = ControlWatcherTiming {
    publication_wait: gents_tools::surface::PUBLICATION_WAIT,
    debounce: CONTROL_RECONCILE_DEBOUNCE,
    local_debounce: CONTROL_RECONCILE_LOCAL_DEBOUNCE,
    settle_retry: CONTROL_RECONCILE_SETTLE_RETRY,
    settle_window: CONTROL_RECONCILE_SETTLE_WINDOW,
    idle_sleep: CONTROL_WATCHER_IDLE_SLEEP,
};

/// Coalesced changes to the control collections, with each subscribed
/// collection id mapped back to its name.
pub(super) struct ControlSubscription {
    changes: gents_store::subscription_source::ChangeSubscription,
    names_by_id: RapidHashMap<String, &'static str>,
}

pub(super) fn subscribe_control_changes(node: &defra_node::Node) -> Result<ControlSubscription> {
    let names = document_view::control_collection_names().collect::<Vec<_>>();
    let ids = gents_store::subscription_source::collection_ids(node, names.iter().copied())?;
    Ok(ControlSubscription {
        changes: gents_store::subscription_source::subscribe(node, ids.clone()),
        names_by_id: ids.into_iter().zip(names).collect(),
    })
}

/// [`subscribe_control_changes`] with the access changes `access` delivers,
/// as on a home whose access settles after its rows commit.
#[cfg(test)]
pub(super) fn subscribe_control_changes_with_access(
    node: &defra_node::Node,
    access: events::Subscription,
) -> Result<ControlSubscription> {
    let names = document_view::control_collection_names().collect::<Vec<_>>();
    let ids = gents_store::subscription_source::collection_ids(node, names.iter().copied())?;
    Ok(ControlSubscription {
        changes: gents_store::subscription_source::ChangeSubscription::new(
            node.subscribe_collection_changes(ids.clone()),
            Some(access),
        ),
        names_by_id: ids.into_iter().zip(names).collect(),
    })
}

pub(super) struct ControlWatcherInputs {
    pub(super) node: Arc<defra_node::Node>,
    pub(super) backend_admission: gents_store::local::backend_protection::BackendAdmission,
    pub(super) subscription: ControlSubscription,
    pub(super) node_did: String,
    pub(super) resolve_context: DocumentResolveContext,
    pub(super) tool_runtime: ToolRuntimeContext,
    pub(super) system_log: bool,
    pub(super) proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    pub(super) runtime_status: RuntimeStatusHandle,
    pub(super) health_events_rx: mpsc::Receiver<()>,
    /// Changes when the supervisor's placement fence asks for a fresh view.
    pub(super) placement_resolves: watch::Receiver<u64>,
    pub(super) shutdown: watch::Receiver<bool>,
}

/// `ControlDebounce.due`: `delay` after the latest wake, but no later than
/// `ceiling` after the first wake the reconcile has not answered.
pub(super) fn due(
    first: tokio::time::Instant,
    now: tokio::time::Instant,
    delay: Duration,
    ceiling: Duration,
) -> tokio::time::Instant {
    (now + delay).min(first + ceiling)
}

/// When the reconcile a wake at `now` schedules runs, remembering the first
/// wake of the pending burst in `pending_since`.
fn after_wake(
    pending_since: &mut Option<tokio::time::Instant>,
    delay: Duration,
    ceiling: Duration,
) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    due(*pending_since.get_or_insert(now), now, delay, ceiling)
}

pub(super) async fn run_control_watcher(inputs: ControlWatcherInputs) -> Result<()> {
    run_control_watcher_with_timing(inputs, CONTROL_WATCHER_TIMING).await
}

pub(super) async fn run_control_watcher_with_timing(
    inputs: ControlWatcherInputs,
    timing: ControlWatcherTiming,
) -> Result<()> {
    let ControlWatcherInputs {
        node,
        backend_admission,
        subscription,
        node_did,
        resolve_context,
        tool_runtime,
        system_log,
        proposals_tx,
        runtime_status,
        mut health_events_rx,
        mut placement_resolves,
        mut shutdown,
    } = inputs;
    backend_admission.validate(&node).await?;
    let mut document_view = document_view::load_document_runtime_view_as(
        &node,
        &node_did,
        resolve_context.identity.did(),
    )
    .await?;
    let sleep = tokio::time::sleep(timing.idle_sleep);
    tokio::pin!(sleep);
    let mut dirty = false;
    let mut pending_visibility = false;
    let mut settle_deadline = None;
    let mut last_proposed_fingerprint = None::<String>;
    let mut last_proposed_basis = None::<PlacementBasis>;
    let mut placement_open = true;
    let mut phase_transition_pending = false;
    let mut resync_required = false;
    let mut replicated_pending = false;
    let mut pending_since = None;
    let mut publication_epoch = None;
    let mut awaiting_release = false;
    let mut recovery_events = runtime_status.publication_recovery();
    let mut recovery_pending = *recovery_events.borrow_and_update() != 0;
    if recovery_pending {
        dirty = true;
        publication_epoch = Some(tokio::time::Instant::now() + timing.publication_wait);
        sleep
            .as_mut()
            .reset(tokio::time::Instant::now() + timing.debounce);
    }
    let ControlSubscription {
        changes: mut subscription,
        names_by_id,
    } = subscription;
    let mut measured_mcp_availability =
        gents_tools::surface::measured_available_mcp_service_ids(&node, &node_did)
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = &mut sleep, if dirty => {
                pending_since = None;
                backend_admission.validate(&node).await?;
                replicated_pending = false;
                if resync_required || pending_visibility || settle_deadline.is_some() {
                    match document_view::load_document_runtime_view_as(
                        &node, &node_did, resolve_context.identity.did()
                    ).await {
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
                let now = tokio::time::Instant::now();
                let deadline = epoch_deadline(publication_epoch, now, timing.publication_wait);
                publication_epoch = Some(deadline);
                let (publication, busy) = match tool_runtime.plugin_store_pin() {
                    None => (None, false),
                    Some(pin) => {
                        let (guard, observed) = match pin.try_shared() {
                            Ok(guard) => (Some(guard), ReadOutcome::Acquired),
                            Err(error) => (None, observed_store(&node_did, &error)),
                        };
                        match watch_store(pin.is_captured(), observed) {
                            StoreWatch::Reconcile => (guard, false),
                            StoreWatch::Contended => (None, true),
                            StoreWatch::Stop => {
                                tracing::info!(node_did = %node_did, ?observed, "stopping control watcher: its plugin store is gone or replaced");
                                return Ok(());
                            }
                        }
                    }
                };
                let disposition = reconcile(busy, now, deadline);
                if disposition == Disposition::KeepPrevious || (busy && awaiting_release) {
                    if disposition == Disposition::KeepPrevious {
                        tracing::debug!(node_did = %node_did, "plugin store is held by an installer; keeping the active runtime generation");
                    }
                    dirty = true;
                    sleep.as_mut().reset(next_publication_poll(now, deadline, timing.settle_retry));
                    continue;
                }
                let mut proposed_update = false;
                let mut resolve_failed = false;
                match async {
                    if publication.is_some() {
                        document_view = document_view::load_document_runtime_view_as(
                            &node, &node_did, resolve_context.identity.did()
                        ).await?;
                    }
                    let mut snapshot = document_view::resolve_document_runtime_snapshot_from_view(
                        &node,
                        &resolve_context,
                        &document_view,
                    )
                    .await?;
                    super::startup::snapshot::demote_unbuildable_agents(system_log, &tool_runtime, &mut snapshot, deadline).await?;
                    anyhow::Ok(snapshot)
                }
                .await
                {
                    Ok(snapshot) => {
                        let fingerprint = snapshot.configuration_fingerprint();
                        let basis = snapshot.placement_basis.clone();
                        let changed = last_proposed_fingerprint.as_deref() != Some(fingerprint.as_str())
                            || last_proposed_basis.as_ref() != Some(&basis);
                        if phase_announced || send_proposal(changed, recovery_pending && disposition == Disposition::Publish) {
                            if !phase_announced {
                                runtime_status
                                    .set_reconcile_phase(ReconcilePhase::Resolving)
                                    .await;
                            }
                            if proposals_tx.send(snapshot).await.is_err() {
                                return Ok(());
                            }
                            last_proposed_fingerprint = Some(fingerprint);
                            last_proposed_basis = Some(basis);
                            proposed_update = true;
                            if disposition == Disposition::Publish {
                                recovery_pending = false;
                            }
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
                if let (Some(pin), Some(_)) = (tool_runtime.plugin_store_pin(), publication.as_ref()) {
                    if store_is_gone(&node_did, pin) {
                        tracing::info!(node_did = %node_did, "stopping control watcher: its plugin store was removed or replaced while it reconciled");
                        return Ok(());
                    }
                }
                drop(publication);
                if resolve_failed {
                    if retry_after_failure(
                        settle_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline),
                        recovery_pending,
                    ) {
                        dirty = true;
                        sleep
                            .as_mut()
                            .reset(tokio::time::Instant::now() + timing.settle_retry);
                    } else {
                        dirty = false;
                        settle_deadline = None;
                        publication_epoch = None;
                        awaiting_release = false;
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
                if disposition == Disposition::PublishDemotingBusy {
                    awaiting_release = true;
                    dirty = true;
                    settle_deadline = None;
                    sleep.as_mut().reset(tokio::time::Instant::now() + timing.settle_retry);
                    continue;
                }
                publication_epoch = None;
                awaiting_release = false;
                dirty = false;
                settle_deadline = None;
                sleep.as_mut().reset(tokio::time::Instant::now() + timing.idle_sleep);
            }
            changed = recovery_events.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                recovery_events.borrow_and_update();
                recovery_pending = true;
                dirty = true;
                publication_epoch = Some(epoch_deadline(publication_epoch, tokio::time::Instant::now(), timing.publication_wait));
                awaiting_release = false;
                sleep.as_mut().reset(after_wake(&mut pending_since, timing.debounce, timing.debounce));
            }
            changed = placement_resolves.changed(), if placement_open => {
                if changed.is_err() {
                    placement_open = false;
                    continue;
                }
                placement_resolves.borrow_and_update();
                resync_required = true;
                dirty = true;
                publication_epoch = Some(epoch_deadline(publication_epoch, tokio::time::Instant::now(), timing.publication_wait));
                awaiting_release = false;
                sleep.as_mut().reset(after_wake(&mut pending_since, timing.local_debounce, timing.debounce));
            }
            Some(()) = health_events_rx.recv() => {
                tracing::info!(
                    node_did = %node_did,
                    "backend measured-health transition detected; scheduling reconcile"
                );
                dirty = true;
                phase_transition_pending = true;
                publication_epoch = Some(epoch_deadline(publication_epoch, tokio::time::Instant::now(), timing.publication_wait));
                awaiting_release = false;
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Debouncing)
                    .await;
                sleep.as_mut().reset(after_wake(&mut pending_since, timing.debounce, timing.debounce));
            }
            batch = subscription.recv() => {
                let Some(batch) = batch else {
                    return Ok(());
                };

                if batch.resync_required {
                    if gents_store::subscription_source::overflowed(&batch) {
                        tracing::warn!(
                            node_did = %node_did,
                            updates = batch.updates,
                            "runtime control watcher document-change capacity exceeded; scheduling full reconcile"
                        );
                    }
                    resync_required = true;
                    dirty = true;
                    phase_transition_pending = true;
                    publication_epoch = Some(epoch_deadline(publication_epoch, tokio::time::Instant::now(), timing.publication_wait));
                    awaiting_release = false;
                    settle_deadline =
                        Some(tokio::time::Instant::now() + timing.settle_window);
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Debouncing)
                        .await;
                    sleep.as_mut().reset(after_wake(&mut pending_since, timing.debounce, timing.debounce));
                    continue;
                }

                for update in batch.changes {
                    let collection_name = names_by_id
                        .get(&update.collection_id)
                        .copied()
                        .unwrap_or(update.collection_id.as_str());
                    if collection_name == "ToolServiceHealthState" {
                        let current = match gents_tools::surface::measured_available_mcp_service_ids(
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
                            match document_view::load_document_runtime_view_as(
                                &node, &node_did, resolve_context.identity.did()
                            ).await {
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
                            match document_view::load_document_runtime_view_as(
                                &node, &node_did, resolve_context.identity.did()
                            ).await {
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
                    publication_epoch = Some(epoch_deadline(publication_epoch, tokio::time::Instant::now(), timing.publication_wait));
                    awaiting_release = false;
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
                    sleep.as_mut().reset(after_wake(&mut pending_since, debounce, timing.debounce));
                }
            }
        }
    }
}

/// The next look at a held plugin store: one retry interval later, but not
/// after the publication epoch expires while it is still open.
fn next_publication_poll(
    now: tokio::time::Instant,
    deadline: tokio::time::Instant,
    retry: Duration,
) -> tokio::time::Instant {
    if now < deadline {
        deadline.min(now + retry)
    } else {
        now + retry
    }
}

/// A store the watcher could not observe at all is weighed as held: an I/O
/// failure is no evidence the store is gone.
fn observed_store(node_did: &str, error: &PublishedPluginReadError) -> ReadOutcome {
    error.outcome().unwrap_or_else(|| {
        tracing::warn!(node_did = %node_did, error = %ErrorChain(error), "plugin store lease is unreadable; treating it as held");
        ReadOutcome::Busy
    })
}

fn store_is_gone(node_did: &str, pin: &StoreReadPin) -> bool {
    let observed = match pin.validate() {
        Ok(()) => ReadOutcome::Acquired,
        Err(error) => observed_store(node_did, &error),
    };
    watch_store(pin.is_captured(), observed) == StoreWatch::Stop
}
