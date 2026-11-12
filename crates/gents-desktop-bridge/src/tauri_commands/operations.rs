use crate::error::BridgeError;

use anyhow::Context;
use std::sync::Arc;

use chrono::Utc;
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::JobControlOutcome;
use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};
use gents_store::config_client::ConfigAccess;
use tauri::State;

use crate::commands::mcp_health::{load_mcp_services_with_health, probe_mcp_service};
use crate::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    BackendHealthView, DesktopInterruptRequest, DesktopJobControlRequest,
    DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest, DesktopProbeMcpServiceRequest,
    DesktopSessionProvenanceRequest, InferenceCallSummaryView, InterruptRequestResult,
    MCPServiceHealthView, McpServiceProbeResult, RuntimeLivenessView, SessionProvenanceView,
};

const BACKGROUND_TOOL_CALL_LIMIT: usize = 256;

const RECENT_CALLS_PER_BACKEND: usize = 10;

#[tauri::command]
pub async fn desktop_operations_snapshot<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, BridgeError>
where
    R: tauri::Runtime,
{
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge not initialized"))?;
    let node_did = state
        .window_command_node(window.label(), request.node_did.as_deref())
        .ok_or_else(|| BridgeError::untyped("no node selected; pass nodeDid explicitly"))?;

    let endpoint = core
        .operator_observation_endpoint(&node_did)
        .await
        .ok_or_else(|| {
            BridgeError::untyped(format!(
                "runtime {node_did} has no authenticated database endpoint"
            ))
        })?;
    let access = ConfigAccess::Graphql(endpoint.clone());
    let (tool_call_rows, status) = tokio::join!(
        fetch_background_tool_calls(&access, &node_did),
        gents_desktop_core::local_runtime::fetch_runtime_status_for_node(&endpoint, &node_did),
    );
    let tool_call_rows = tool_call_rows?;
    let liveness = status.and_then(|status| {
        serde_json::from_value::<RuntimeLivenessView>(
            status
                .get("liveness")
                .cloned()
                .context("runtime status has no liveness observation")?,
        )
        .context("decoding runtime liveness")
    });
    let (liveness, liveness_unavailable_reason) = match liveness {
        Ok(liveness) => (Some(liveness), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let unavailable = RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: Vec::new(),
        active_native_executors_available: false,
        active_native_executors: Vec::new(),
    };
    let backgrounded_tools =
        project_backgrounded_tools(&tool_call_rows, liveness.as_ref().unwrap_or(&unavailable));
    let stuck_diagnostics = stuck_diagnostics_from_tool_calls(&tool_call_rows);

    Ok(DesktopOperationsSnapshot {
        fetched_at: Utc::now().to_rfc3339(),
        node_did: Some(node_did),
        liveness,
        liveness_unavailable_reason,
        backgrounded_tools,
        stuck_diagnostics,
    })
}

async fn fetch_background_tool_calls(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Vec<ToolCallRow>, BridgeError> {
    let mut view = gents_session::views::transcript::ToolCallRows::background(
        gents_session::views::transcript::Scope::Node(node_did.to_owned()),
    );
    view.limit = Some(BACKGROUND_TOOL_CALL_LIMIT as u64);
    let rows = access
        .read_view(&view)
        .await
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;
    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            tool_call_doc_id: row.doc_id,
            request_id: row.request_id.unwrap_or_default(),
            tool_call_id: row.tool_call_id.unwrap_or_default(),
            tool_name: row.tool_name.unwrap_or_default(),
            lifecycle_state: row.lifecycle_state,
            status: row.status,
            started_at: row.started_at,
            completed_at: row.completed_at,
            deadline_at: row.deadline_at,
            await_mode: row.await_mode,
            stuck_since: row.stuck_since,
            job: row.job_pin.is_some(),
        })
        .collect())
}

#[cfg(test)]
mod background_tool_query_tests {
    use super::*;

    #[test]
    fn runtime_liveness_decodes_native_names_and_keeps_product_names() {
        let view: RuntimeLivenessView = serde_json::from_value(serde_json::json!({
            "expired_processing_count": 0,
            "requests": [],
            "active_tool_calls": [],
            "active_native_executors_available": true,
            "active_native_executors": [{ "id": 7, "pid": 42, "argv0": "runtime-task", "tool_name": "Bash", "started_at": "2026-11-04T00:00:00Z", "age_ms": 10 }]
        })).unwrap();
        assert_eq!(view.active_native_executors[0].pid, 42);
        let product = serde_json::to_value(view).unwrap();
        assert_eq!(product["activeNativeExecutors"][0]["toolName"], "Bash");
        assert!(product.get("active_native_executors").is_none());
    }
}

#[tauri::command]
pub async fn desktop_session_provenance<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
    mut request: DesktopSessionProvenanceRequest,
) -> Result<SessionProvenanceView, BridgeError>
where
    R: tauri::Runtime,
{
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge has not finished bootstrapping"))?;
    request.node_did = state.window_command_node(window.label(), request.node_did.as_deref());
    crate::provenance::session_provenance_request(&core, request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_interrupt_request<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
    mut request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, BridgeError>
where
    R: tauri::Runtime,
{
    let core = crate::state::current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    request.node_did = state.window_command_node(window.label(), request.node_did.as_deref());
    tracing::info!(
        target: "gents_desktop::interrupt",
        request_id = %request.request_id,
        node_did = %request.node_did.as_deref().unwrap_or(""),
        "desktop interrupt action received"
    );
    let result = crate::interrupt::interrupt_request(&core, &request).await;
    match &result {
        Ok(result) => tracing::info!(
            target: "gents_desktop::interrupt",
            request_id = %result.request_id,
            accepted = result.accepted,
            already_interrupted = result.already_interrupted,
            interrupt_requested_at = %result.interrupt_requested_at.as_deref().unwrap_or(""),
            "desktop interrupt action completed"
        ),
        Err(error) => tracing::warn!(
            target: "gents_desktop::interrupt",
            request_id = %request.request_id,
            node_did = %request.node_did.as_deref().unwrap_or(""),
            error = %error,
            "desktop interrupt action failed"
        ),
    }
    result
}

#[tauri::command]
pub async fn desktop_job_control(
    state: State<'_, DesktopAppState>,
    request: DesktopJobControlRequest,
) -> Result<JobControlOutcome, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    tracing::info!(
        target: "gents_desktop::job_control",
        tool_call_doc_id = %request.tool_call_doc_id,
        action = ?request.action,
        "desktop job control received"
    );
    let result = crate::job_control::job_control(&core, &request).await;
    match &result {
        Ok(outcome) => tracing::info!(
            target: "gents_desktop::job_control",
            tool_call_doc_id = %request.tool_call_doc_id,
            outcome = ?outcome,
            "desktop job control completed"
        ),
        Err(error) => tracing::warn!(
            target: "gents_desktop::job_control",
            tool_call_doc_id = %request.tool_call_doc_id,
            %error,
            "desktop job control failed"
        ),
    }
    result
}

#[tauri::command]
pub async fn desktop_process_control(
    state: State<'_, DesktopAppState>,
    request: OperatorProcessControlRequest,
) -> Result<ProcessControlOutcome, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    crate::process_control::process_control(&core, &request).await
}

#[tauri::command]
pub async fn desktop_list_backends_with_health<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<BackendHealthView>, BridgeError>
where
    R: tauri::Runtime,
{
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_backends_with_health_for_core(core, state.window_command_node(window.label(), None)).await
}

/// The backends of `node_did`, the calling window's node.
pub async fn list_backends_with_health_for_core(
    core: Arc<ClientCore>,
    node_did: Option<String>,
) -> Result<Vec<BackendHealthView>, BridgeError> {
    let node_did = node_did.ok_or_else(|| BridgeError::untyped("no runtime selected"))?;
    let endpoint = core
        .operator_observation_endpoint(&node_did)
        .await
        .ok_or_else(|| {
            BridgeError::untyped(format!(
                "runtime {node_did} has no authenticated database endpoint"
            ))
        })?;
    let access = ConfigAccess::Graphql(endpoint);
    let backends = gents_inference::views::backends::read_inventory(
        &access,
        &gents_inference::views::backends::BackendList::new(&node_did),
    )
    .await
    .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;

    let mut views = Vec::with_capacity(backends.len());
    for row in backends {
        let backend = row
            .metadata
            .into_backend()
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        let observation = row.observation;
        let recent_calls = fetch_recent_calls(&access, &node_did, &backend.backend_id)
            .await
            .map_err(|err| err.to_string())?;
        let probe_status = observation
            .as_ref()
            .and_then(|row| row.probe_status.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let display_state = observation
            .as_ref()
            .map(|row| row.display_state(backend.enabled))
            .unwrap_or_else(|| {
                if backend.enabled {
                    "unknown"
                } else {
                    "disabled"
                }
            })
            .to_string();
        let display_state = serde_json::from_value(serde_json::Value::String(display_state))
            .map_err(|error| format!("invalid backend display state: {error}"))?;
        let catalog_scope = backend
            .auth
            .uses_node_oauth()
            .then_some(backend.node_did.as_str());
        let models = observation
            .as_ref()
            .map(|row| row.catalog_for(catalog_scope))
            .transpose()
            .map_err(|err| err.to_string())?
            .flatten()
            .map(|catalog| {
                catalog
                    .models
                    .iter()
                    .map(|model| model.model_name.clone())
                    .collect()
            })
            .unwrap_or_default();
        views.push(BackendHealthView {
            backend_id: backend.backend_id,
            name: backend.name,
            provider_kind: backend.provider_kind.as_str().to_string(),
            endpoint: backend.endpoint,
            enabled: backend.enabled,
            probe_status,
            display_state,
            last_probe: observation.and_then(|row| row.last_probe),
            max_concurrent: backend.max_concurrent.unwrap_or(1),
            max_queue_depth: backend.max_queue_depth.unwrap_or(100),
            models,
            recent_calls,
        });
    }
    Ok(views)
}

async fn fetch_recent_calls(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
) -> Result<Vec<InferenceCallSummaryView>, anyhow::Error> {
    let rows = access
        .read_view(
            &gents_inference::views::activity::InferenceCalls::new(node_did)
                .recent_backend(backend_id)
                .limit(RECENT_CALLS_PER_BACKEND as u64),
        )
        .await
        .with_context(|| format!("list InferenceCall for backend {backend_id}"))?;
    Ok(rows.into_iter().map(parse_call_row).collect())
}

fn parse_call_row(
    row: gents_inference::views::activity::InferenceCallRow,
) -> InferenceCallSummaryView {
    InferenceCallSummaryView {
        call_id: row.call_id,
        call_seq: row.call_seq.unwrap_or_default(),
        call_kind: row.call_kind.unwrap_or_default(),
        call_state: row.call_state,
        failure_reason: row.failure_reason,
        queued_at: row.queued_at,
        started_at: row.started_at,
        ended_at: row.ended_at,
        queue_depth_at_enqueue: row.queue_depth_at_enqueue,
        prompt_tokens: row.usage.prompt_tokens,
        completion_tokens: row.usage.completion_tokens,
        cached_input_tokens: row.usage.cached_input_tokens,
        cache_creation_input_tokens: row.usage.cache_creation_input_tokens,
        reasoning_output_tokens: row.usage.reasoning_output_tokens,
        provider_total_tokens: row.usage.provider_total_tokens,
        usage_completeness: row.usage.usage_completeness.map(|value| match value {
            gents_protocol::usage::UsageCompleteness::Missing => "missing".to_owned(),
            gents_protocol::usage::UsageCompleteness::Partial => "partial".to_owned(),
            gents_protocol::usage::UsageCompleteness::Complete => "complete".to_owned(),
        }),
    }
}

#[tauri::command]
pub async fn desktop_list_mcp_services_with_health<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<MCPServiceHealthView>, BridgeError>
where
    R: tauri::Runtime,
{
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_mcp_services_with_health_for_core(core, state.window_command_node(window.label(), None))
        .await
}

/// The MCP services of `node_did`, the calling window's node.
pub async fn list_mcp_services_with_health_for_core(
    core: Arc<ClientCore>,
    node_did: Option<String>,
) -> Result<Vec<MCPServiceHealthView>, BridgeError> {
    load_mcp_services_with_health(core.as_ref(), node_did.as_deref())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_probe_mcp_service(
    state: State<'_, DesktopAppState>,
    request: DesktopProbeMcpServiceRequest,
) -> Result<McpServiceProbeResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    probe_mcp_service_for_core(core, request).await
}

pub(crate) async fn probe_mcp_service_for_core(
    core: Arc<ClientCore>,
    request: DesktopProbeMcpServiceRequest,
) -> Result<McpServiceProbeResult, BridgeError> {
    probe_mcp_service(core.as_ref(), &request.node_did, &request.service_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_background_tool(
    state: State<'_, DesktopAppState>,
    request: crate::types::DesktopBackgroundToolRequest,
) -> Result<bool, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    crate::commands::background_tool(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
