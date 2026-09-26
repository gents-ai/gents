use crate::error::BridgeError;

use anyhow::Context;
use std::sync::Arc;

use chrono::Utc;
use gents::backend_registry::{list_all_backends, lookup_backend_observation};
use gents::defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_desktop_core::client::ClientCore;
use tauri::State;

use crate::commands::mcp_health::{load_mcp_services_with_health, probe_mcp_service};
use crate::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    BackendHealthView, DesktopInterruptRequest, DesktopOperationsSnapshot,
    DesktopOperationsSnapshotRequest, DesktopProbeMcpServiceRequest,
    DesktopSessionProvenanceRequest, InferenceCallSummaryView, InterruptRequestResult,
    MCPServiceHealthView, McpServiceProbeResult, NativeExecutorStatusView, RuntimeLivenessView,
    SessionProvenanceView,
};

const BACKGROUND_TOOL_CALL_LIMIT: usize = 256;

const RECENT_CALLS_PER_BACKEND: usize = 10;

#[tauri::command]
pub async fn desktop_operations_snapshot(
    state: State<'_, DesktopAppState>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge not initialized"))?;
    let agent_did = request
        .agent_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| core.selected_agent_did())
        .ok_or_else(|| BridgeError::untyped("no agent selected; pass agentDid explicitly"))?;

    let native_executors: Vec<NativeExecutorStatusView> =
        gents::native_executor_status::active_native_executors()
            .into_iter()
            .map(|ne| NativeExecutorStatusView {
                id: ne.id as i64,
                pid: ne.pid as u32,
                argv0: ne.argv0,
                tool_name: ne.tool_name,
                started_at: ne.started_at,
                age_ms: ne.age_ms,
            })
            .collect();

    let tool_call_rows = fetch_background_tool_calls(&core, &agent_did)
        .await
        .map_err(|e| format!("failed to query AgentToolCall: {e}"))?;

    let liveness = RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: Vec::new(),
        active_native_executors_available: true,
        active_native_executors: native_executors,
    };

    let backgrounded_tools = project_backgrounded_tools(&tool_call_rows, &liveness);
    let stuck_diagnostics = stuck_diagnostics_from_tool_calls(&tool_call_rows);

    Ok(DesktopOperationsSnapshot {
        fetched_at: Utc::now().to_rfc3339(),
        agent_did: Some(agent_did),
        liveness: Some(liveness),
        liveness_unavailable_reason: None,
        backgrounded_tools,
        stuck_diagnostics,
    })
}

async fn fetch_background_tool_calls(
    core: &Arc<ClientCore>,
    agent_did: &str,
) -> Result<Vec<ToolCallRow>, BridgeError> {
    let query = background_tool_calls_query(agent_did);

    let response = graphql_with_transaction_retry(&core.node(), &query, "background tool calls")
        .await
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;

    let data = response
        .data
        .ok_or_else(|| BridgeError::untyped("AgentToolCall query returned no data"))?;
    let rows = data
        .get("AgentToolCall")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            request_id: row
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_call_id: row
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            tool_name: row
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            lifecycle_state: row
                .get("lifecycle_state")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            status: row
                .get("status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            started_at: row
                .get("started_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            deadline_at: row
                .get("deadline_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            await_mode: row
                .get("await_mode")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            stuck_since: row
                .get("stuck_since")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
        .collect())
}

fn background_tool_calls_query(agent_did: &str) -> String {
    let escaped_agent_did = escape_graphql_string(agent_did);

    format!(
        r#"
        query {{
            AgentToolCall(
                filter: {{
                    await_mode: {{ _eq: "background" }},
                    lifecycle_state: {{ _in: ["pending", "running"] }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }}
                }},
                limit: {BACKGROUND_TOOL_CALL_LIMIT}
            ) {{
                request_id
                tool_call_id
                tool_name
                lifecycle_state
                status
                started_at
                deadline_at
                await_mode
                stuck_since
            }}
        }}
    "#
    )
}

#[cfg(test)]
mod background_tool_query_tests {
    use super::*;

    #[test]
    fn scopes_live_rows_to_the_selected_agent_and_caps_the_scan() {
        let query = background_tool_calls_query("did:key:z6Mk\"selected");

        assert!(query.contains(r#"await_mode: { _eq: "background" }"#));
        assert!(query.contains(r#"lifecycle_state: { _in: ["pending", "running"] }"#));
        assert!(query.contains(r#"agent_did: { _eq: "did:key:z6Mk\"selected" }"#));
        assert!(query.contains(&format!("limit: {BACKGROUND_TOOL_CALL_LIMIT}")));
    }
}

#[tauri::command]
pub async fn desktop_session_provenance(
    state: State<'_, DesktopAppState>,
    request: DesktopSessionProvenanceRequest,
) -> Result<SessionProvenanceView, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge has not finished bootstrapping"))?;
    let agent_did = request
        .agent_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| core.selected_agent_did())
        .ok_or_else(|| BridgeError::untyped("no agent selected; pass agentDid explicitly"))?;
    crate::provenance::session_provenance(&core, &agent_did, &request.session_id)
        .await
        .map_err(BridgeError::untyped)
}

#[tauri::command]
pub async fn desktop_interrupt_request(
    state: State<'_, DesktopAppState>,
    request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, BridgeError> {
    let core = crate::state::current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    tracing::info!(
        target: "gents_desktop::interrupt",
        request_id = %request.request_id,
        agent_did = %request.agent_did.as_deref().unwrap_or(""),
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
            agent_did = %request.agent_did.as_deref().unwrap_or(""),
            error,
            "desktop interrupt action failed"
        ),
    }
    result.map_err(BridgeError::untyped)
}

#[tauri::command]
pub async fn desktop_list_backends_with_health(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<BackendHealthView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_backends_with_health_for_core(core).await
}

pub async fn list_backends_with_health_for_core(
    core: Arc<ClientCore>,
) -> Result<Vec<BackendHealthView>, BridgeError> {
    let node = core.node();
    let backends = list_all_backends(node)
        .await
        .map_err(|err| err.to_string())?;

    let mut views = Vec::with_capacity(backends.len());
    for backend in backends {
        let observation = lookup_backend_observation(node, &backend.agent_did, &backend.backend_id)
            .await
            .map_err(|err| err.to_string())?;
        let recent_calls = fetch_recent_calls(node, &backend.backend_id)
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
        let catalog_scope = matches!(
            backend.auth,
            gents::document_config::BackendAuth::PrincipalOAuth
        )
        .then_some(backend.agent_did.as_str());
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
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Vec<InferenceCallSummaryView>, anyhow::Error> {
    let escaped_id = escape_graphql_string(backend_id);
    let query = format!(
        r#"query {{
            InferenceCall(
                filter: {{ backend_id: {{ _eq: "{escaped_id}" }} }},
                order: {{ queued_at: DESC }},
                limit: {limit}
            ) {{
                call_id
                call_seq
                call_kind
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                queue_depth_at_enqueue
                prompt_tokens
                completion_tokens
            }}
        }}"#,
        limit = RECENT_CALLS_PER_BACKEND,
    );

    let resp = graphql_with_transaction_retry(&node, &query, "list InferenceCall by backend")
        .await
        .with_context(|| format!("list InferenceCall for backend {backend_id}"))?;

    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(|value| value.as_array())
        .map(|rows| rows.iter().map(parse_call_row).collect::<Vec<_>>())
        .unwrap_or_default())
}

fn parse_call_row(row: &serde_json::Value) -> InferenceCallSummaryView {
    InferenceCallSummaryView {
        call_id: row
            .get("call_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        call_seq: row.get("call_seq").and_then(|v| v.as_i64()).unwrap_or(0),
        call_kind: row
            .get("call_kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        call_state: row
            .get("call_state")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        failure_reason: row
            .get("failure_reason")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        queued_at: row
            .get("queued_at")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        started_at: row
            .get("started_at")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        ended_at: row
            .get("ended_at")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned),
        queue_depth_at_enqueue: row.get("queue_depth_at_enqueue").and_then(|v| v.as_i64()),
        prompt_tokens: row.get("prompt_tokens").and_then(|v| v.as_i64()),
        completion_tokens: row.get("completion_tokens").and_then(|v| v.as_i64()),
    }
}

#[tauri::command]
pub async fn desktop_list_mcp_services_with_health(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<MCPServiceHealthView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    list_mcp_services_with_health_for_core(core).await
}

pub async fn list_mcp_services_with_health_for_core(
    core: Arc<ClientCore>,
) -> Result<Vec<MCPServiceHealthView>, BridgeError> {
    load_mcp_services_with_health(core.as_ref())
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
    probe_mcp_service(core.as_ref(), &request.service_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
