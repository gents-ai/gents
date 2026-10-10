#[path = "snapshot/projection.rs"]
pub mod projection;
#[path = "snapshot/timeline.rs"]
mod timeline;
#[path = "snapshot/tool_presentation.rs"]
mod tool_presentation;

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use gents::agent::p2p_reconcile::session_hydration::{
    ClientHydrationPhase, ClientHydrationProgress, SESSION_OWNERSHIP_MISMATCH,
};
use gents_desktop_core::client::{
    load_peer_records, project_sync_health, ClientCore, ClientSyncStateSnapshot, DesktopPaths,
    P2PHealth, PairingCollectionStatus, SyncHealth,
};
use gents_desktop_core::remote_admin::PairingErrorClass;

use super::state::ResolvedBridgePolicy;
use super::types::{
    normalize_optional, DesktopBootstrapSummary, DesktopClientSnapshot, P2PHealthView,
    PairingCollectionStatusView, SavedPeerView, SessionHydrationView, SyncHealthView,
};
use projection::{project_bootstrap_summary, project_client_snapshot, SnapshotGrants};

#[derive(Debug, serde::Deserialize)]
struct StoredInitConfigView {
    #[serde(default)]
    node_name: Option<String>,
    #[serde(default)]
    node_did: Option<String>,
    #[serde(default)]
    tool_ceiling: Option<String>,
    #[serde(default)]
    tool_root: Option<String>,
}

fn to_health_view(health: &P2PHealth) -> P2PHealthView {
    P2PHealthView {
        status: health.status_label().to_string(),
        connected_peer_count: health.connected_peer_count,
        replicator_count: health.replicator_count,
        consecutive_failures: health.consecutive_failures,
        last_error: health.last_error.clone(),
        last_ok_at: system_time_rfc3339(health.last_ok_at),
        last_failure_at: system_time_rfc3339(health.last_failure_at),
    }
}

/// A refusal because another requester owns the session is not a sync
/// failure to retry: it is presented as a session this client cannot read.
pub(crate) fn to_hydration_view(
    progress: &ClientHydrationProgress,
    rejection_detail: Option<String>,
) -> SessionHydrationView {
    let ownership_refused = progress.phase == ClientHydrationPhase::Failed
        && rejection_detail.as_deref() == Some(SESSION_OWNERSHIP_MISMATCH);
    SessionHydrationView {
        session_id: progress.session_id.clone(),
        node_did: progress.node_did.clone(),
        phase: if ownership_refused {
            "unreadable".to_string()
        } else {
            progress.phase.as_str().to_string()
        },
        merged_count: progress.merged_count,
        covered_count: progress.covered_count,
        served_count: progress.served_count,
        detail: if ownership_refused {
            Some(UNREADABLE_OWNERSHIP_REASON.to_string())
        } else {
            rejection_detail.filter(|_| progress.phase == ClientHydrationPhase::Failed)
        },
    }
}

const UNREADABLE_OWNERSHIP_REASON: &str =
    "The node reports that this session belongs to another requester, so this client cannot read it.";

/// A session whose replicated header already shows this client cannot read
/// it. No hydration request is made for it (`SessionHydration.canStart`).
pub(crate) fn unreadable_hydration_view(
    session_id: &str,
    node_did: &str,
    reason: &str,
) -> SessionHydrationView {
    SessionHydrationView {
        session_id: session_id.to_string(),
        node_did: node_did.to_string(),
        phase: "unreadable".to_string(),
        merged_count: 0,
        covered_count: 0,
        served_count: None,
        detail: Some(reason.to_string()),
    }
}

pub(crate) fn to_sync_health_view(health: &SyncHealth) -> SyncHealthView {
    SyncHealthView {
        state: health.state.as_str().to_string(),
        last_error: health.last_error.clone(),
        connected_peer_count: health.connected_peer_count,
        pending_dag_count: health.pending_dag_count,
        persisted_pending_dag_count: health.persisted_pending_dag_count,
        push_retry_marker_count: health.push_retry_marker_count,
        exhausted_fetch_count: health.exhausted_fetch_count,
        quarantined_dag_count: health.quarantined_dag_count,
    }
}

pub(crate) fn to_pairing_collection_view(
    status: &PairingCollectionStatus,
) -> PairingCollectionStatusView {
    PairingCollectionStatusView {
        collection_id: status.collection_id.clone(),
        pairing_retry_count: status.pairing_retry_count,
        last_retry_at: system_time_rfc3339(status.last_retry_at),
        last_retry_error_class: pairing_error_class_label(status.last_retry_error_class),
        stuck_since: system_time_rfc3339(status.stuck_since),
    }
}

pub(crate) fn project_client_sync_health(sync: &ClientSyncStateSnapshot) -> Option<SyncHealthView> {
    project_sync_health(sync).map(|health| to_sync_health_view(&health))
}

fn pairing_error_class_label(class: Option<PairingErrorClass>) -> Option<String> {
    class.map(|class| format!("{class:?}"))
}

pub(crate) fn system_time_rfc3339(value: Option<SystemTime>) -> Option<String> {
    value.map(|value| DateTime::<Utc>::from(value).to_rfc3339())
}

pub async fn build_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    let node_home = gents_desktop_core::local_runtime::default_node_home()
        .map_err(|error| error.to_string())?;
    let desktop_paths = DesktopPaths::discover().map_err(|error| error.to_string())?;
    let full = build_bootstrap_summary_raw(&desktop_paths, Some(node_home.as_path())).await?;
    Ok(project_bootstrap_summary(full, SnapshotGrants::all()))
}

pub async fn build_bootstrap_summary_for_policy(
    policy: &ResolvedBridgePolicy,
) -> Result<DesktopBootstrapSummary, String> {
    let full =
        build_bootstrap_summary_raw(&policy.desktop_paths, policy.node_home.as_deref()).await?;
    Ok(project_bootstrap_summary(full, policy.snapshot_grants))
}

async fn build_bootstrap_summary_raw(
    desktop_paths: &DesktopPaths,
    node_home: Option<&Path>,
) -> Result<DesktopBootstrapSummary, String> {
    let peer_records = load_peer_records(&desktop_paths.peer_directory_path())
        .await
        .map_err(|error| error.to_string())?;
    let init = node_home.and_then(read_stored_init_config);
    let node_home_display = node_home
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let node_home_exists = node_home.map(|p| p.exists()).unwrap_or(false);

    Ok(DesktopBootstrapSummary {
        default_node_home: node_home_display,
        init_node_name: init
            .as_ref()
            .and_then(|config| normalize_optional(config.node_name.as_deref())),
        init_node_did: init
            .as_ref()
            .and_then(|config| normalize_optional(config.node_did.as_deref())),
        init_tool_ceiling: init
            .as_ref()
            .and_then(|config| normalize_optional(config.tool_ceiling.as_deref())),
        init_tool_root: init
            .as_ref()
            .and_then(|config| normalize_optional(config.tool_root.as_deref())),
        desktop_home: desktop_paths.root().display().to_string(),
        peer_directory_path: desktop_paths.peer_directory_path().display().to_string(),
        node_data_dir: desktop_paths.node_data_dir().display().to_string(),
        diagnostics_hint: crate::logging::diagnostics_hint(desktop_paths.root()),
        node_home_exists,
        desktop_home_exists: desktop_paths.root().exists(),
        peer_directory_exists: desktop_paths.peer_directory_path().exists(),
        client_state_exists: desktop_paths.client_state_exists(),
        saved_peers: peer_records
            .iter()
            .map(|peer| SavedPeerView {
                peer_id: peer.peer_id.clone(),
                label: peer.label.clone(),
                node_did: peer.node_did.clone(),
                addr: peer.addr.clone(),
                source: peer.source.clone(),
                graphql: peer.graphql.clone(),
            })
            .collect(),
    })
}

fn read_stored_init_config(node_home: &std::path::Path) -> Option<StoredInitConfigView> {
    let bytes = std::fs::read(node_home.join("init.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[path = "snapshot/runtime_tasks.rs"]
mod runtime_tasks;
#[cfg(test)]
use runtime_tasks::{recent_runs_for_task_views, session_summaries, task_run_history};
use runtime_tasks::{request_matches_node, source_matches_node};

#[path = "snapshot/runtime.rs"]
mod runtime;
pub use runtime::build_runtime_snapshot;

#[path = "snapshot/operations_snapshot.rs"]
pub mod operations_snapshot;

#[path = "snapshot/session.rs"]
mod session;
pub use session::apply_session_timeline_page;
pub use session::apply_session_timeline_page_with_query;
pub use session::attach_last_request_context;
pub use session::build_session_live_delta;
#[cfg(test)]
pub(crate) use session::build_session_live_delta_from_store;
pub use session::build_session_snapshot_for_node_with_transcript;
#[cfg(test)]
pub use session::build_session_snapshot_from_store;
#[cfg(test)]
pub use session::build_session_snapshot_from_store_for_node;

pub async fn build_client_snapshot_with_grants(
    core: Option<&Arc<ClientCore>>,
    policy: Option<&ResolvedBridgePolicy>,
    grants: SnapshotGrants,
) -> Result<DesktopClientSnapshot, String> {
    let bootstrap = match policy {
        Some(policy) => {
            build_bootstrap_summary_raw(&policy.desktop_paths, policy.node_home.as_deref()).await?
        }
        None => {
            let node_home = gents_desktop_core::local_runtime::default_node_home()
                .map_err(|error| error.to_string())?;
            let desktop_paths = DesktopPaths::discover().map_err(|error| error.to_string())?;
            build_bootstrap_summary_raw(&desktop_paths, Some(node_home.as_path())).await?
        }
    };
    let client = match core {
        Some(core) => Some(build_runtime_snapshot(core.as_ref()).await),
        None => None,
    };
    Ok(project_client_snapshot(
        DesktopClientSnapshot { bootstrap, client },
        grants,
    ))
}
#[cfg(test)]
#[path = "snapshot/tests.rs"]
mod tests;
