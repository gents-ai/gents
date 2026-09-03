use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{watch, OwnedRwLockReadGuard, RwLock};

use crate::lifecycle::{ExecutionOrigin, RequestLifecycle};
use crate::runtime_snapshot::{
    effective_behavior_admission, ActiveRuntimeSnapshot, EffectiveBehaviorAdmission,
};
use crate::runtime_status::RuntimeStatusHandle;
use crate::watcher::{AgentRequest, DefraWatcher, Watcher};

use super::context::BehaviorResolution;

#[derive(Clone)]
pub(super) struct RuntimeAdmissionGate {
    state: Arc<RwLock<bool>>,
    changed: watch::Sender<bool>,
    #[cfg(test)]
    entered: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    dispatch_attempted: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

impl RuntimeAdmissionGate {
    pub(super) fn closed() -> Self {
        let (changed, _) = watch::channel(false);
        Self {
            state: Arc::new(RwLock::new(false)),
            changed,
            #[cfg(test)]
            entered: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            dispatch_attempted: None,
        }
    }

    #[cfg(test)]
    pub(super) fn closed_with_dispatch_probe(
        dispatch_attempted: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    ) -> Self {
        let mut gate = Self::closed();
        gate.dispatch_attempted = dispatch_attempted;
        gate
    }

    pub(super) async fn open(&self) {
        *self.state.write().await = true;
        self.changed.send_replace(true);
    }

    pub(super) async fn close(&self) {
        // Announce closure before waiting for in-progress routing admissions.
        // A router may hold the read-side lease while an executor queue is
        // full; that blocked send selects on this watch and releases the lease
        // so the write-side drain cannot deadlock behind it.
        self.changed.send_replace(false);
        *self.state.write().await = false;
    }

    async fn wait_open(&self, shutdown: &mut watch::Receiver<bool>) -> bool {
        let mut changed = self.changed.subscribe();
        loop {
            if *changed.borrow_and_update() {
                return true;
            }
            tokio::select! {
                biased;
                _ = shutdown.changed() => return false,
                result = changed.changed() => {
                    if result.is_err() {
                        return false;
                    }
                }
            }
        }
    }

    async fn enter(&self) -> Option<OwnedRwLockReadGuard<bool>> {
        let guard = self.state.clone().read_owned().await;
        if !*guard {
            return None;
        }
        #[cfg(test)]
        self.entered.notify_one();
        Some(guard)
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<bool> {
        self.changed.subscribe()
    }

    #[cfg(test)]
    pub(super) async fn is_open(&self) -> bool {
        *self.state.read().await
    }

    #[cfg(test)]
    pub(super) async fn wait_for_entry_for_test(&self) {
        self.entered.notified().await;
    }
}

pub(super) async fn run_router(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: String,
    local_deployment_id: String,
    active_snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    mut shutdown: watch::Receiver<bool>,
    admission_gate: RuntimeAdmissionGate,
    runtime_status: RuntimeStatusHandle,
) -> Result<()> {
    if *shutdown.borrow() {
        return Ok(());
    }
    if !admission_gate.wait_open(&mut shutdown).await {
        return Ok(());
    }
    let watcher =
        DefraWatcher::new(node.clone(), &agent_did).with_local_deployment_id(local_deployment_id);
    let result = run_router_with_watcher(
        node,
        agent_did,
        watcher,
        active_snapshot_rx,
        shutdown,
        admission_gate.clone(),
        runtime_status,
    )
    .await;
    if result.is_err() {
        // A router failure is an admission failure. Close locally before the
        // task reports to the runtime coordinator so no concurrent watcher can
        // dispatch against a readiness row whose router acknowledgement failed.
        admission_gate.close().await;
    }
    result
}

pub(super) async fn run_router_with_watcher<W>(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: String,
    mut watcher: W,
    mut active_snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    mut shutdown: watch::Receiver<bool>,
    admission_gate: RuntimeAdmissionGate,
    runtime_status: RuntimeStatusHandle,
) -> Result<()>
where
    W: Watcher,
{
    let mut active_snapshot = active_snapshot_rx.borrow().clone();
    let mut admission_changed = admission_gate.subscribe();
    let mut readiness_changed = runtime_status.readiness().subscribe_observation();

    loop {
        let Some((request, routed_snapshot, admission_observation)) =
            wait_for_next_request_with_latest_snapshot(
                &agent_did,
                &mut watcher,
                &mut active_snapshot,
                &mut active_snapshot_rx,
                &mut shutdown,
                &admission_gate,
                &mut admission_changed,
                &mut readiness_changed,
                Some(&runtime_status),
            )
            .await?
        else {
            return Ok(());
        };

        let Some(_admission) = admission_gate.enter().await else {
            return Ok(());
        };

        let resolution = resolve_behavior_for_request(
            node.as_ref(),
            &request,
            routed_snapshot.default_behavior_id.as_str(),
        )
        .await?;
        if let Some(reason) = resolution.rejection_reason.as_deref() {
            tracing::warn!(
                request_id = %request.request_id,
                session_id = %request.session_id,
                behavior_id = %resolution.behavior_id,
                reason = %reason,
                "rejecting request before dispatch"
            );
            fail_routed_request(
                node.clone(),
                agent_did.as_str(),
                request,
                resolution.behavior_id.as_str(),
                reason,
            )
            .await?;
            continue;
        }

        let startup_diagnostic = admission_observation.demotion_reason(&resolution.behavior_id);
        match effective_behavior_admission(
            routed_snapshot
                .dispatchers
                .contains_key(&resolution.behavior_id),
            routed_snapshot
                .unavailable_behaviors
                .get(&resolution.behavior_id),
            startup_diagnostic.as_deref(),
        ) {
            EffectiveBehaviorAdmission::Ready => {
                let dispatcher = routed_snapshot
                    .dispatchers
                    .get(&resolution.behavior_id)
                    .expect("effective admission verified the dispatcher");
                tracing::info!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %resolution.behavior_id,
                    "dispatching request to behavior executor"
                );
                #[cfg(test)]
                if let Some(dispatch_attempted) = &admission_gate.dispatch_attempted {
                    let _ = dispatch_attempted.send(());
                }
                let sent = tokio::select! {
                    biased;
                    changed = admission_changed.changed() => {
                        if changed.is_err() || !*admission_changed.borrow_and_update() {
                            return Ok(());
                        }
                        continue;
                    }
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow_and_update() {
                            return Ok(());
                        }
                        continue;
                    }
                    sent = dispatcher.send(request) => sent,
                };
                sent.map_err(|_| {
                    anyhow!(
                        "executor queue for behavior {} closed unexpectedly",
                        resolution.behavior_id
                    )
                })?;
            }
            EffectiveBehaviorAdmission::Unavailable {
                public_reason,
                diagnostic,
            } => {
                tracing::warn!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %resolution.behavior_id,
                    public_reason = ?public_reason,
                    diagnostic = %diagnostic,
                    "behavior unavailable for request"
                );
                fail_routed_request(
                    node.clone(),
                    agent_did.as_str(),
                    request,
                    resolution.behavior_id.as_str(),
                    public_reason.public_message(),
                )
                .await?;
            }
            EffectiveBehaviorAdmission::Unassigned => {
                tracing::warn!(
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    behavior_id = %resolution.behavior_id,
                    agent_did = %agent_did,
                    "behavior is not assigned to the active runtime"
                );
                fail_routed_request(
                    node.clone(),
                    agent_did.as_str(),
                    request,
                    resolution.behavior_id.as_str(),
                    "behavior is not assigned to this runtime",
                )
                .await?;
            }
        }
    }
}

pub(super) async fn wait_for_next_request_with_latest_snapshot<W>(
    agent_did: &str,
    watcher: &mut W,
    active_snapshot: &mut Arc<ActiveRuntimeSnapshot>,
    active_snapshot_rx: &mut watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    shutdown: &mut watch::Receiver<bool>,
    admission_gate: &RuntimeAdmissionGate,
    admission_changed: &mut watch::Receiver<bool>,
    readiness_changed: &mut watch::Receiver<
        crate::behavior_readiness_publisher::BehaviorAdmissionObservation,
    >,
    runtime_status: Option<&RuntimeStatusHandle>,
) -> Result<
    Option<(
        AgentRequest,
        Arc<ActiveRuntimeSnapshot>,
        crate::behavior_readiness_publisher::BehaviorAdmissionObservation,
    )>,
>
where
    W: Watcher,
{
    let mut pending_request = None;
    loop {
        let routed_snapshot = active_snapshot_rx.borrow_and_update().clone();
        let readiness_observation = readiness_changed.borrow_and_update().clone();
        *active_snapshot = routed_snapshot.clone();
        let readiness_aligned =
            readiness_observation.source_generation() == routed_snapshot.generation;
        if !readiness_aligned {
            tokio::select! {
                biased;

                _ = shutdown.changed() => return Ok(None),
                changed = admission_changed.changed() => {
                    if changed.is_err() || !*admission_changed.borrow_and_update() {
                        return Ok(None);
                    }
                }
                changed = active_snapshot_rx.changed() => {
                    if changed.is_err() {
                        return Ok(None);
                    }
                }
                changed = readiness_changed.changed() => {
                    if changed.is_err() {
                        return Ok(None);
                    }
                }
            }
            continue;
        }
        if let Some(runtime_status) = runtime_status {
            if let Err(error) = runtime_status
                .publish_router_generation(routed_snapshot.generation)
                .await
                .with_context(|| {
                    format!(
                        "durably acknowledge router generation {}",
                        routed_snapshot.generation
                    )
                })
            {
                admission_gate.close().await;
                return Err(error);
            }
        }
        if let Some(request) = pending_request.take() {
            return Ok(Some((request, routed_snapshot, readiness_observation)));
        }
        let request = tokio::select! {
            biased;

            _ = shutdown.changed() => return Ok(None),
            changed = admission_changed.changed() => {
                if changed.is_err() || !*admission_changed.borrow_and_update() {
                    return Ok(None);
                }
                continue;
            }
            changed = active_snapshot_rx.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
                *active_snapshot = active_snapshot_rx.borrow_and_update().clone();
                continue;
            }
            changed = readiness_changed.changed() => {
                if changed.is_err() {
                    return Ok(None);
                }
                continue;
            }
            req = watcher.next_request() => {
                match req {
                    Some(Ok(req)) => req,
                    Some(Err(error)) => {
                        tracing::error!(agent_did = %agent_did, error = %error, "watcher error, retrying");
                        continue;
                    }
                    None => return Ok(None),
                }
            }
        };
        pending_request = Some(request);
    }
}

pub(super) async fn resolve_behavior_for_request(
    node: &defra_node::EmbeddedNode,
    request: &AgentRequest,
    default_behavior_id: &str,
) -> Result<BehaviorResolution> {
    let requested_behavior_id =
        normalize_optional_string(request.behavior_id.as_deref()).map(ToOwned::to_owned);
    let session_behavior_id =
        crate::session::load_session_behavior_id(node, &request.session_id).await?;
    let behavior_id = requested_behavior_id
        .clone()
        .or_else(|| session_behavior_id.clone())
        .unwrap_or_else(|| default_behavior_id.to_string());

    let rejection_reason = match (
        session_behavior_id.as_deref(),
        requested_behavior_id.as_deref(),
    ) {
        (Some(existing), Some(requested)) if existing != requested => Some(format!(
            "session {} is pinned to behavior {} and cannot switch to {}",
            request.session_id, existing, requested
        )),
        _ => None,
    };

    Ok(BehaviorResolution {
        behavior_id,
        rejection_reason,
    })
}

pub(super) async fn fail_routed_request(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: &str,
    request: AgentRequest,
    behavior_id: &str,
    error_message: &str,
) -> Result<()> {
    let execution_origin =
        match ExecutionOrigin::from_persisted(request.execution_origin.as_deref()) {
            Ok(origin) => origin,
            Err(error) => {
                let reason = format!("request admission denied: {error:#}");
                return crate::request_admission::terminalize_pending_request_rejection(
                    node.as_ref(),
                    &request.doc_id,
                    agent_did,
                    &reason,
                    "terminalize_invalid_execution_origin_before_route_rejection",
                )
                .await;
            }
        };
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        behavior_id,
        agent_did,
        request.clone(),
        Duration::from_secs(30).as_secs(),
        execution_origin,
        "",
    );

    lifecycle.reject_admission(error_message).await
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(in crate::agent) fn default_hostname() -> String {
    hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

pub(super) fn format_pending_visibility_error(details: &[String]) -> String {
    if details.is_empty() {
        return "waiting for referenced control documents to become visible".to_string();
    }
    format!(
        "waiting for referenced control documents to become visible: {}",
        details.join("; ")
    )
}
