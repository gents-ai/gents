use super::port::matching_external_server;
use crate::local_pairing::{
    ensure_managed_runtime_pairing, observe_managed_runtime_schema, retry_managed_pairing,
    ManagedPairingTarget, PairingFailure, MANAGED_PAIRING_ATTEMPTS, MANAGED_PAIRING_RETRY_DELAY,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::ManagedServerStatus;
use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::OperatorIdentityInteraction;
use std::sync::Arc;

pub(super) async fn pairing_is_ready(state: &DesktopAppState, node_did: Option<&str>) -> bool {
    let (Some(core), Some(node_did)) = (current_core(state), node_did) else {
        return false;
    };
    core.peer_records().await.iter().any(|peer| {
        peer.node_did == node_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    })
}

pub(in crate::tauri_commands) async fn start_running_managed_pairing(
    state: &DesktopAppState,
    core: Arc<ClientCore>,
) {
    let Some(node_home) = state.policy.node_home.clone() else {
        tracing::warn!("managed pairing requires a local node home");
        return;
    };
    let target =
        match matching_external_server(&node_home, OperatorIdentityInteraction::Prevent).await {
            Ok(status) => status.as_ref().and_then(pairing_target),
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::managed_server",
                    error = %error,
                    "failed to inspect native managed runtime for background pairing"
                );
                None
            }
        };
    let Some(target) = target else { return };
    if let Err(error) = core
        .refresh_local_standard_peer(&node_home, &target.node_name)
        .await
    {
        tracing::warn!(
            target: "gents_desktop::managed_server",
            node_did = %target.node_did,
            error = %error,
            "failed to refresh native managed runtime route before pairing"
        );
        return;
    }
    start_managed_runtime_pairing(state, core, node_home, target).await;
}

pub(super) async fn start_managed_runtime_pairing(
    state: &DesktopAppState,
    core: Arc<ClientCore>,
    node_home: std::path::PathBuf,
    target: ManagedPairingTarget,
) {
    {
        let mut managed = state.managed_server.lock().await;
        if !managed
            .schema_observation_task
            .as_ref()
            .is_some_and(|task| !task.inner().is_finished())
        {
            let observed_core = Arc::clone(&core);
            let observed_target = target.clone();
            managed.schema_observation_task = Some(tauri::async_runtime::spawn(async move {
                if let Err(error) =
                    observe_managed_runtime_schema(&observed_core, &observed_target).await
                {
                    tracing::warn!(
                        target: "gents_desktop::managed_server",
                        node_did = %observed_target.node_did,
                        error = %error,
                        "managed runtime replicated schema observation failed"
                    );
                }
            }));
        }
    }

    if core.peer_records().await.iter().any(|peer| {
        peer.node_did == target.node_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    }) {
        return;
    }

    let mut managed = state.managed_server.lock().await;
    if managed
        .pairing_task
        .as_ref()
        .is_some_and(|task| !task.inner().is_finished())
    {
        return;
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    managed.pairing_cancel = Some(cancel.clone());
    managed.pairing_task = Some(tauri::async_runtime::spawn(async move {
        let paired = retry_managed_pairing(
            &cancel,
            MANAGED_PAIRING_ATTEMPTS,
            MANAGED_PAIRING_RETRY_DELAY,
            || ensure_managed_runtime_pairing(Arc::clone(&core), &node_home, &target, &cancel),
        )
        .await;
        match paired {
            Ok(()) => {}
            Err(PairingFailure::Cancelled) => {
                tracing::info!(
                    target: "gents_desktop::managed_server",
                    node_did = %target.node_did,
                    "background managed runtime pairing was cancelled"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::managed_server",
                    node_did = %target.node_did,
                    error = %error,
                    "background managed runtime pairing failed"
                );
            }
        }
    }));
}

pub(super) fn pairing_target(status: &ManagedServerStatus) -> Option<ManagedPairingTarget> {
    Some(ManagedPairingTarget {
        node_name: status.node_name.as_deref()?.trim().to_string(),
        node_did: status.node_did.as_deref()?.trim().to_string(),
        graphql: status.graphql.as_deref()?.trim().to_string(),
    })
    .filter(|target| {
        !target.node_name.is_empty() && !target.node_did.is_empty() && !target.graphql.is_empty()
    })
}

pub(in crate::tauri_commands) async fn drain_managed_runtime_pairing(state: &DesktopAppState) {
    let task = {
        let mut managed = state.managed_server.lock().await;
        if let Some(observation) = managed.schema_observation_task.take() {
            observation.abort();
        }
        if let Some(cancel) = managed.pairing_cancel.take() {
            cancel.cancel();
        }
        managed.pairing_task.take()
    };
    if let Some(task) = task {
        if let Err(error) = task.await {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                error = %error,
                "managed pairing task failed while draining"
            );
        }
    }
}
