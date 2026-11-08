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
use gents_desktop_core::client::{
    load_peer_records, project_sync_health, ClientCore, ClientSyncStateSnapshot, DesktopPaths,
    P2PHealth, SyncHealth,
};

use super::config::ResolvedBridgePolicy;
use super::types::{
    normalize_optional, DesktopBootstrapSummary, DesktopClientSnapshot, InitConfigStatus,
    P2PHealthView, SavedPeerView, SyncHealthView,
};
use projection::{project_client_snapshot, SnapshotGrants};

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

pub(crate) fn to_sync_health_view(health: &SyncHealth) -> SyncHealthView {
    SyncHealthView {
        state: health.state.as_str().to_string(),
        last_error: health.last_error.clone(),
        connected_peer_count: health.connected_peer_count,
        activation_error_count: health.activation_error_count,
        pending_dag_count: health.pending_dag_count,
        persisted_pending_dag_count: health.persisted_pending_dag_count,
        push_retry_marker_count: health.push_retry_marker_count,
        exhausted_fetch_count: health.exhausted_fetch_count,
        quarantined_dag_count: health.quarantined_dag_count,
    }
}

pub(crate) fn project_client_sync_health(sync: &ClientSyncStateSnapshot) -> Option<SyncHealthView> {
    project_sync_health(sync).map(|health| to_sync_health_view(&health))
}

pub(crate) fn system_time_rfc3339(value: Option<SystemTime>) -> Option<String> {
    value.map(|value| DateTime::<Utc>::from(value).to_rfc3339())
}

async fn build_bootstrap_summary_raw(
    desktop_paths: &DesktopPaths,
    node_home: Option<&Path>,
) -> Result<DesktopBootstrapSummary, String> {
    let peer_records = load_peer_records(desktop_paths.peer_directory_path())
        .await
        .map_err(|error| error.to_string())?;
    let (init, init_config_status) = node_home
        .map(read_stored_init_config)
        .unwrap_or((None, InitConfigStatus::Absent));
    let node_home_display = node_home
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let node_home_exists = node_home.map(|p| p.exists()).unwrap_or(false);

    Ok(DesktopBootstrapSummary {
        default_node_home: node_home_display,
        init_node_name: init
            .as_ref()
            .and_then(|config| normalize_optional(Some(config.node_name.as_str()))),
        init_node_did: init
            .as_ref()
            .and_then(|config| normalize_optional(Some(config.node_did.as_str()))),
        init_config_status: Some(init_config_status),
        init_tool_ceiling: init
            .as_ref()
            .and_then(|config| normalize_optional(Some(config.tool_ceiling.as_str()))),
        init_tool_root: init
            .as_ref()
            .and_then(|config| normalize_optional(config.tool_root.as_deref())),
        desktop_home: desktop_paths.root().display().to_string(),
        diagnostics_hint: crate::logging::diagnostics_hint(desktop_paths.root()),
        node_home_exists,
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

fn read_stored_init_config(
    node_home: &std::path::Path,
) -> (
    Option<gents::store::local::home::StoredInitConfig<String, String>>,
    InitConfigStatus,
) {
    let path = gents::store::local::home::init_config_path(node_home);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (None, InitConfigStatus::Absent);
        }
        Err(_) => return (None, InitConfigStatus::Unreadable),
    };
    let config = match serde_json::from_reader::<
        _,
        gents::store::local::home::StoredInitConfig<String, String>,
    >(file)
    {
        Ok(config) if !config.node_did.trim().is_empty() => config,
        _ => return (None, InitConfigStatus::Unreadable),
    };
    (Some(config), InitConfigStatus::Configured)
}

pub(crate) fn resolve_node_home_tool_ceiling(
    node_home: &std::path::Path,
) -> Result<(gents::ToolCeiling, &'static str), String> {
    let Some(config) = read_init_ceiling(node_home) else {
        return Ok((gents::ToolCeiling::meta_only(), "default_no_init_json"));
    };

    let tool_root = config
        .tool_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    match config
        .tool_ceiling
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("metaonly") | Some("meta_only") => {
            Ok((gents::ToolCeiling::meta_only(), "init_json"))
        }
        Some("readonly") => Ok((
            match tool_root.as_deref() {
                Some(root) => gents::ToolCeiling::readonly_at(std::path::PathBuf::from(root)),
                None => gents::ToolCeiling::readonly(),
            },
            "init_json",
        )),
        Some("readwrite") => {
            let root = tool_root.ok_or_else(|| {
                "init.json has readwrite tool_ceiling but no tool_root".to_string()
            })?;
            Ok((
                gents::ToolCeiling::readwrite(std::path::PathBuf::from(root)),
                "init_json",
            ))
        }
        Some(other) => Err(format!("init.json has unrecognized tool_ceiling {other:?}")),
    }
}

#[derive(serde::Deserialize)]
struct InitCeilingView {
    #[serde(default)]
    tool_ceiling: Option<String>,
    #[serde(default)]
    tool_root: Option<String>,
}

fn read_init_ceiling(node_home: &std::path::Path) -> Option<InitCeilingView> {
    let bytes = std::fs::read(node_home.join("init.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[path = "snapshot/runtime_tasks.rs"]
mod runtime_tasks;
#[path = "snapshot/task_lineage.rs"]
mod task_lineage;
#[cfg(test)]
use runtime_tasks::{recent_runs_for_task_views, task_run_histories};
use runtime_tasks::{request_matches_node, source_matches_node};
pub(crate) use runtime_tasks::{
    session_summaries, session_summary_order_key, SessionSummaryOrderKey,
};

#[path = "snapshot/runtime.rs"]
mod runtime;
pub use runtime::build_runtime_snapshot;
pub(crate) use runtime::node_reads;

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
pub(crate) use session::inference_activity::read_activity as read_session_inference_activity;
pub use session::load_session_context_details;
#[cfg(test)]
pub(crate) use session::render_transcript_rows;
pub(crate) use session::{attach_context_history_rows, render_transcript_rows_with_provenance};

pub async fn build_client_snapshot_with_grants(
    core: Option<&Arc<ClientCore>>,
    policy: &ResolvedBridgePolicy,
    grants: SnapshotGrants,
) -> Result<DesktopClientSnapshot, String> {
    let bootstrap =
        build_bootstrap_summary_raw(&policy.desktop_paths, policy.node_home.as_deref()).await?;
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
