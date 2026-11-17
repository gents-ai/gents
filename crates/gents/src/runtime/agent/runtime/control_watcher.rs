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

pub(super) const CONTROL_RECONCILE_DEBOUNCE: Duration = Duration::from_secs(5);
pub(super) const CONTROL_RECONCILE_LOCAL_DEBOUNCE: Duration = Duration::from_millis(250);
pub(super) const CONTROL_RECONCILE_SETTLE_RETRY: Duration = Duration::from_secs(1);
const CONTROL_RECONCILE_SETTLE_WINDOW: Duration = Duration::from_secs(60);
const CONTROL_WATCHER_IDLE_SLEEP: Duration = Duration::from_secs(60 * 60 * 24 * 365);

#[derive(Clone, Copy)]
pub(super) struct ControlWatcherTiming {
    pub(super) publication_wait: Duration,
    pub(super) debounce: Duration,
    pub(super) local_debounce: Duration,
    pub(super) settle_retry: Duration,
    pub(super) settle_window: Duration,
    pub(super) idle_sleep: Duration,
}

const CONTROL_WATCHER_TIMING: ControlWatcherTiming = ControlWatcherTiming {
    publication_wait: Duration::from_secs(60),
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

pub(super) fn subscribe_control_changes(node: &defra_node::Node) -> Result<ControlSubscription> {
    let names = document_view::control_collection_names().collect::<Vec<_>>();
    let ids = gents_store::subscription_source::collection_ids(node, names.iter().copied())?;
    Ok(ControlSubscription {
        changes: node.subscribe_collection_changes(ids.clone()),
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
    pub(super) proposals_tx: mpsc::Sender<ResolvedRuntimeSnapshot>,
    pub(super) runtime_status: RuntimeStatusHandle,
    pub(super) health_events_rx: mpsc::Receiver<()>,
    pub(super) shutdown: watch::Receiver<bool>,
}

pub(super) async fn run_control_watcher(inputs: ControlWatcherInputs) -> Result<()> {
    run_control_watcher_with_timing(inputs, CONTROL_WATCHER_TIMING).await
}

pub(super) async fn run_control_watcher_with_timing(
    inputs: ControlWatcherInputs,
    timing: ControlWatcherTiming,
) -> Result<()> {
    run_control_watcher_inner(
        inputs,
        timing,
        #[cfg(test)]
        None,
    )
    .await
}

async fn run_control_watcher_inner(
    inputs: ControlWatcherInputs,
    timing: ControlWatcherTiming,
    #[cfg(test)] probe: Option<WatcherTestProbe>,
) -> Result<()> {
    let ControlWatcherInputs {
        node,
        backend_admission,
        subscription,
        node_did,
        resolve_context,
        tool_runtime,
        proposals_tx,
        runtime_status,
        mut health_events_rx,
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
    let mut recovery_events = runtime_status.publication_recovery();
    let mut recovery_pending = *recovery_events.borrow_and_update() != 0;
    let mut dirty = recovery_pending;
    if dirty {
        sleep
            .as_mut()
            .reset(tokio::time::Instant::now() + timing.debounce);
    }
    let mut pending_visibility = false;
    let mut settle_deadline = None;
    let mut publication_deadline = None;
    let mut last_proposed_fingerprint = None::<String>;
    let mut phase_transition_pending = false;
    let mut resync_required = false;
    let mut replicated_pending = false;
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
            biased;
            _ = shutdown.changed() => return Ok(()),
            _ = &mut sleep, if dirty => {
                backend_admission.validate(&node).await?;
                replicated_pending = false;
                if resync_required || pending_visibility || settle_deadline.is_some() {
                    let reload = async {
                        #[cfg(test)] if let Some(probe) = &probe { probe.fail_once(WatcherFailure::Reload)?; }
                        document_view::load_document_runtime_view_as(&node, &node_did, resolve_context.identity.did()).await
                    }.await;
                    match reload {
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
                                publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                                sleep.as_mut().reset(publication_retry_at(publication_deadline, timing.settle_retry));
                            } else {
                                dirty = false;
                                settle_deadline = None;
                                clear_publication_epoch(&mut publication_deadline);
                                #[cfg(test)] if let Some(probe) = &probe { let _ = probe.events.send(WatcherTestEvent::FailedIdle(publication_deadline)); }
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
                let mut publication_busy = false;
                let deadline = *publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                #[cfg(test)] if let Some(probe) = &probe { let _ = probe.events.send(WatcherTestEvent::Attempt(deadline, tokio::time::Instant::now())); }

                let resolution = async {
                    #[cfg(test)] if let Some(probe) = &probe { probe.fail_once(WatcherFailure::Resolve)?; }
                    let publication = match gents_tools::surface::publication_guard_until(tool_runtime.plugin_home(), deadline).await {
                        Ok(guard) => guard,
                        Err(error) if super::startup::snapshot::preserve_generation_for_surface_error(&error) => {
                            publication_busy = true;
                            if defer_authority(true, tokio::time::Instant::now() < deadline) {
                                return Err(error);
                            }
                            // Budget exhausted: authority is no longer contingent
                            // on plugin publication. Resolve latest config now;
                            // busy plugin surfaces become unavailable with cause.
                            None
                        }
                        Err(error) => return Err(error),
                    };
                    if tool_runtime.plugin_home().is_some() {
                        document_view = document_view::load_document_runtime_view_as(&node, &node_did, resolve_context.identity.did()).await?;
                    }
                    let mut snapshot = document_view::resolve_document_runtime_snapshot_from_view(
                        &node, &resolve_context, &document_view,
                    ).await?;
                    super::startup::snapshot::demote_unbuildable_agents_until(
                        &tool_runtime, &mut snapshot, deadline,
                        defer_authority(publication_busy, tokio::time::Instant::now() < deadline),
                    ).await?;
                    drop(publication);
                    anyhow::Ok(snapshot)
                };
                let resolved = tokio::select! {
                    biased;
                    _ = shutdown.changed() => return Ok(()),
                    resolved = resolution => resolved,
                };
                match resolved {
                    Ok(snapshot) => {
                        let fingerprint = snapshot.configuration_fingerprint();
                        if phase_announced || send_recovered_proposal(last_proposed_fingerprint.as_deref() != Some(fingerprint.as_str()), recovery_pending) {
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
                            if !publication_busy { recovery_pending = false; }
                        }
                    }
                    Err(error) => {
                        resolve_failed = true;
                        publication_busy = super::startup::snapshot::preserve_generation_for_surface_error(&error);
                        tracing::error!(
                            node_did = %node_did,
                            error = %error,
                            "runtime reconcile resolve failed; keeping previous active generation"
                        );
                        runtime_status.publish_error(&format!("{error:#}")).await;
                    }
                }
                if resolve_failed {
                    if retry_proposal(publication_busy, settle_deadline
                        .is_some_and(|deadline| tokio::time::Instant::now() < deadline))
                    {
                        dirty = true;
                        publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                        sleep
                            .as_mut()
                            .reset(publication_retry_at(publication_deadline, timing.settle_retry));
                    } else {
                        dirty = false;
                        settle_deadline = None;
                        clear_publication_epoch(&mut publication_deadline);
                        #[cfg(test)] if let Some(probe) = &probe { let _ = probe.events.send(WatcherTestEvent::FailedIdle(publication_deadline)); }
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
                dirty = publication_busy;
                settle_deadline = None;
                if !publication_busy { clear_publication_epoch(&mut publication_deadline); }
                sleep.as_mut().reset(tokio::time::Instant::now() + if publication_busy { timing.settle_retry } else { timing.idle_sleep });
            }
            result = recovery_events.changed() => {
                if result.is_err() { return Ok(()); }
                recovery_events.borrow_and_update();
                recovery_pending = true;
                dirty = true;
                publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                sleep.as_mut().reset(publication_debounce_at(publication_deadline, timing.debounce));
            }
            Some(()) = health_events_rx.recv() => {
                tracing::info!(
                    node_did = %node_did,
                    "backend measured-health transition detected; scheduling reconcile"
                );
                dirty = true;
                publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                phase_transition_pending = true;
                runtime_status
                    .set_reconcile_phase(ReconcilePhase::Debouncing)
                    .await;
                sleep.as_mut().reset(publication_debounce_at(publication_deadline, timing.debounce));
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
                    publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                    phase_transition_pending = true;
                    settle_deadline =
                        Some(tokio::time::Instant::now() + timing.settle_window);
                    runtime_status
                        .set_reconcile_phase(ReconcilePhase::Debouncing)
                        .await;
                    sleep.as_mut().reset(publication_debounce_at(publication_deadline, timing.debounce));
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
                                    publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                                    sleep.as_mut().reset(
                                        publication_retry_at(publication_deadline, timing.settle_retry),
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
                                    publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
                                    sleep.as_mut().reset(
                                        publication_retry_at(publication_deadline, timing.settle_retry),
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
                    publication_deadline.get_or_insert_with(|| tokio::time::Instant::now() + timing.publication_wait);
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
                    sleep.as_mut().reset(publication_debounce_at(publication_deadline, debounce));
                }
            }
        }
    }
}

fn retry_proposal(publication_busy: bool, within_settle_window: bool) -> bool {
    publication_busy || within_settle_window
}

#[cfg(test)]
#[test]
fn publication_retry_outlives_the_settle_window() {
    gents_lean_vocab::families::runtime_reconcile_surface_publication_retry_proposal::replay(|i| {
        retry_proposal(i.busy, i.within_settle_window)
    });
}

fn defer_authority(busy: bool, budget_remaining: bool) -> bool {
    busy && budget_remaining
}

#[cfg(test)]
#[test]
fn bounded_authority_deferral_replays_the_lean_owner() {
    gents_lean_vocab::families::runtime_reconcile_surface_publication_defer_authority::replay(
        |i| defer_authority(i.busy, i.budget_remaining),
    );
}

fn publication_retry_at(
    deadline: Option<tokio::time::Instant>,
    retry: Duration,
) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    let retry_at = now + retry;
    deadline
        .filter(|deadline| *deadline > now)
        .map_or(retry_at, |deadline| deadline.min(retry_at))
}

#[cfg(test)]
#[tokio::test(start_paused = true)]
async fn publication_retry_cannot_sleep_past_the_epoch_budget_or_spin_after_expiry() {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    assert_eq!(
        publication_retry_at(Some(deadline), Duration::from_secs(5)),
        deadline
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        publication_retry_at(Some(deadline), Duration::from_secs(5)),
        deadline + Duration::from_secs(5)
    );
}

fn clear_publication_epoch(deadline: &mut Option<tokio::time::Instant>) {
    *deadline = None;
}

fn publication_debounce_at(
    deadline: Option<tokio::time::Instant>,
    debounce: Duration,
) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    (now + debounce).min(deadline.unwrap_or(now + debounce))
}

#[cfg(test)]
#[tokio::test(start_paused = true)]
async fn failed_reconcile_idle_clears_the_budget_before_the_next_event() {
    let mut deadline = Some(tokio::time::Instant::now());
    tokio::time::advance(Duration::from_secs(70)).await;
    clear_publication_epoch(&mut deadline);
    assert!(deadline.is_none());
    let now = tokio::time::Instant::now();
    let next = *deadline.get_or_insert_with(|| now + Duration::from_secs(60));
    assert_eq!(
        publication_debounce_at(deadline, Duration::from_millis(250)),
        now + Duration::from_millis(250)
    );
    assert_eq!(next - now, Duration::from_secs(60));
}

fn send_recovered_proposal(changed: bool, recovery_pending: bool) -> bool {
    changed || recovery_pending
}

#[cfg(test)]
#[test]
fn busy_park_wake_is_not_fingerprint_deduplicated() {
    gents_lean_vocab::families::runtime_reconcile_surface_publication_send_recovered_proposal::replay(|i| send_recovered_proposal(i.changed, i.recovery_pending));
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum WatcherFailure {
    Reload,
    Resolve,
}
#[cfg(test)]
enum WatcherTestEvent {
    FailedIdle(Option<tokio::time::Instant>),
    Attempt(tokio::time::Instant, tokio::time::Instant),
}
#[cfg(test)]
struct WatcherTestProbe {
    failure: std::sync::Mutex<Option<WatcherFailure>>,
    events: mpsc::UnboundedSender<WatcherTestEvent>,
}
#[cfg(test)]
impl WatcherTestProbe {
    fn fail_once(&self, stage: WatcherFailure) -> Result<()> {
        let mut failure = self.failure.lock().unwrap();
        if *failure == Some(stage) {
            *failure = None;
            anyhow::bail!("injected single watcher operation failure");
        }
        Ok(())
    }
}

#[cfg(test)]
#[tokio::test]
async fn failed_reload_and_resolve_call_sites_reset_the_next_event_epoch() {
    use super::tests::support::*;
    for stage in [WatcherFailure::Reload, WatcherFailure::Resolve] {
        let endpoint = MockModelEndpoint::start("default").unwrap();
        let fixture = plugin_runtime_fixture(
            "failed-watcher-epoch",
            "backend-failed-watcher-epoch",
            endpoint.endpoint(),
        )
        .await
        .unwrap();
        let agent = crate::Gents::from_default_agent_documents(
            fixture.node.clone(),
            fixture.runtime.clone(),
            crate::runtime::agent::DocumentRuntimeOptions {
                backend_admission: Some(fixture.admission()),
                tool_ceiling: gents_tools::surface::ToolCeiling::meta_only(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let context = agent.document_runtime_context().cloned().unwrap();
        let owner = fixture.runtime.did().to_owned();
        let status = RuntimeStatusHandle::new(fixture.node.clone(), &owner);
        status
            .initialize_startup(agent.default_agent_did())
            .await
            .unwrap();
        let runtime = ToolRuntimeContext::new_with_node_did(
            fixture.node.clone(),
            Default::default(),
            Default::default(),
            "localhost",
            None,
            &owner,
            Some(context.identity.clone()),
        )
        .with_plugins(context.plugins.clone());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (health_tx, health_rx) = mpsc::channel(1);
        let (proposals_tx, mut proposals_rx) = mpsc::channel(4);
        let (events, mut observed) = mpsc::unbounded_channel();
        let timing = ControlWatcherTiming {
            publication_wait: Duration::from_millis(100),
            debounce: Duration::from_millis(10),
            local_debounce: Duration::from_millis(10),
            settle_retry: Duration::from_millis(1),
            settle_window: Duration::ZERO,
            idle_sleep: Duration::from_secs(3600),
        };
        let watcher = tokio::spawn(run_control_watcher_inner(
            ControlWatcherInputs {
                node: fixture.node.clone(),
                backend_admission: fixture.admission(),
                subscription: subscribe_control_changes(&fixture.node).unwrap(),
                node_did: owner.clone(),
                resolve_context: context,
                tool_runtime: runtime,
                proposals_tx,
                runtime_status: status,
                health_events_rx: health_rx,
                shutdown: shutdown_rx,
            },
            timing,
            Some(WatcherTestProbe {
                failure: std::sync::Mutex::new(Some(stage)),
                events,
            }),
        ));
        match stage {
            WatcherFailure::Reload => merge_fixture_document(
                &fixture.user_access,
                "test.failed_epoch.reload",
                gents_store::collection::Collection::AgentContext,
                &owner,
                &format!("{}:context", agent.default_agent_did()),
                serde_json::json!({"system_prompt":"reload to exercise the failed-idle call site"}),
            )
            .await,
            WatcherFailure::Resolve => health_tx.send(()).await.unwrap(),
        }
        tokio::time::timeout(crate::support::live_inference::STALL_CEILING, async {
            loop {
                if let WatcherTestEvent::FailedIdle(deadline) = observed.recv().await.unwrap() {
                    assert!(
                        deadline.is_none(),
                        "the actual failed-idle branch kept its stale epoch"
                    );
                    break;
                }
            }
        })
        .await
        .unwrap();
        let sent = tokio::time::Instant::now();
        health_tx.send(()).await.unwrap();
        let (deadline, attempt) =
            tokio::time::timeout(crate::support::live_inference::STALL_CEILING, async {
                loop {
                    if let WatcherTestEvent::Attempt(deadline, attempt) =
                        observed.recv().await.unwrap()
                    {
                        break (deadline, attempt);
                    }
                }
            })
            .await
            .unwrap();
        assert!(
            attempt >= sent + timing.debounce,
            "the next event lost its debounce"
        );
        assert!(
            deadline >= sent + timing.publication_wait,
            "the next event inherited the expired wait budget"
        );
        assert!(proposals_rx.recv().await.is_some());
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(crate::support::live_inference::STALL_CEILING, watcher)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
