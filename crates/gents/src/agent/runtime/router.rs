use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
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
}

impl RuntimeAdmissionGate {
    pub(super) fn closed() -> Self {
        let (changed, _) = watch::channel(false);
        Self {
            state: Arc::new(RwLock::new(false)),
            changed,
        }
    }

    pub(super) async fn open(&self) {
        *self.state.write().await = true;
        self.changed.send_replace(true);
    }

    pub(super) async fn close(&self) {
        // The write guard waits for every in-progress routing admission to
        // leave its read-side critical section before shutdown is published.
        *self.state.write().await = false;
        self.changed.send_replace(false);
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
        (*guard).then_some(guard)
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.changed.subscribe()
    }

    #[cfg(test)]
    pub(super) async fn is_open(&self) -> bool {
        *self.state.read().await
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
    run_router_with_watcher(
        node,
        agent_did,
        watcher,
        active_snapshot_rx,
        shutdown,
        admission_gate,
        runtime_status,
    )
    .await
}

async fn run_router_with_watcher<W>(
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
                dispatcher.send(request).await.map_err(|_| {
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
            runtime_status
                .publish_router_generation(routed_snapshot.generation)
                .await;
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

async fn fail_routed_request(
    node: Arc<defra_node::EmbeddedNode>,
    agent_did: &str,
    request: AgentRequest,
    behavior_id: &str,
    error_message: &str,
) -> Result<()> {
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        behavior_id,
        agent_did,
        request.clone(),
        Duration::from_secs(30).as_secs(),
        ExecutionOrigin::from_persisted(request.execution_origin.as_deref()),
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
