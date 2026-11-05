use crate::error::BridgeError;

use anyhow::Context;
use std::sync::Arc;

use chrono::Utc;
use gents::inference::backends::registry::lookup_backend_observation_in_txn;
use gents::store::config_client::{list_inference_backends_in_txn, ConfigAccess};
use gents::store::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_protocol::job::JobControlOutcome;
use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};
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
pub async fn desktop_operations_snapshot(
    state: State<'_, DesktopAppState>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge not initialized"))?;
    let node_did = request
        .node_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| core.selected_node_did())
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
    let query = background_tool_calls_query(node_did);

    let response = access
        .execute(&query)
        .await
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;

    let data = response
        .get("data")
        .ok_or_else(|| BridgeError::untyped("AgentToolCall query returned no data"))?;
    let rows = data
        .get("AgentToolCall")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            tool_call_doc_id: row
                .get("_docID")
                .and_then(|v| v.as_str())
                .map(str::to_string),
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
            completed_at: row
                .get("completed_at")
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
            job: row.get("job_pin").is_some_and(|pin| !pin.is_null()),
        })
        .collect())
}

fn background_tool_calls_query(node_did: &str) -> String {
    let escaped_node_did = escape_graphql_string(node_did);

    format!(
        r#"
        query {{
            AgentToolCall(
                filter: {{
                    await_mode: {{ _eq: "background" }},
                    lifecycle_state: {{ _in: ["pending", "running"] }},
                    node_did: {{ _eq: "{escaped_node_did}" }}
                }},
                limit: {BACKGROUND_TOOL_CALL_LIMIT}
            ) {{
                _docID
                request_id
                tool_call_id
                tool_name
                lifecycle_state
                status
                started_at
                completed_at
                deadline_at
                await_mode
                stuck_since
                job_pin
            }}
        }}
    "#
    )
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

    #[test]
    fn background_query_selects_job_pin() {
        let query = background_tool_calls_query("did:key:z6Mk");

        assert!(query.contains("job_pin"));
        assert!(!query.contains("job_step_state"));
    }
}

#[tauri::command]
pub async fn desktop_session_provenance(
    state: State<'_, DesktopAppState>,
    request: DesktopSessionProvenanceRequest,
) -> Result<SessionProvenanceView, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge has not finished bootstrapping"))?;
    crate::provenance::session_provenance_request(&core, request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
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
            error,
            "desktop interrupt action failed"
        ),
    }
    result.map_err(|error| BridgeError::untyped(error.to_string()))
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
            error,
            "desktop job control failed"
        ),
    }
    result.map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_process_control(
    state: State<'_, DesktopAppState>,
    request: OperatorProcessControlRequest,
) -> Result<ProcessControlOutcome, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop bridge core not initialized"))?;
    crate::process_control::process_control(&core, &request)
        .await
        .map_err(BridgeError::untyped)
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
    let node_did = core
        .selected_node_did()
        .ok_or_else(|| BridgeError::untyped("no runtime selected"))?;
    let endpoint = core
        .operator_observation_endpoint(&node_did)
        .await
        .ok_or_else(|| {
            BridgeError::untyped(format!(
                "runtime {node_did} has no authenticated database endpoint"
            ))
        })?;
    let access = ConfigAccess::Graphql(endpoint);
    let backends = access
        .transact_readonly("desktop.backend_health", |txn| {
            let node_did = &node_did;
            Box::pin(async move {
                let backends = list_inference_backends_in_txn(txn, node_did).await?;
                let mut observations = Vec::with_capacity(backends.len());
                for backend in backends {
                    let observation =
                        lookup_backend_observation_in_txn(txn, node_did, &backend.backend_id)
                            .await?;
                    observations.push((backend, observation));
                }
                Ok::<_, anyhow::Error>(observations)
            })
        })
        .await
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;

    let mut views = Vec::with_capacity(backends.len());
    for (backend, observation) in backends {
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
    let escaped_id = escape_graphql_string(backend_id);
    let escaped_node = escape_graphql_string(node_did);
    let query = format!(
        r#"query {{
            InferenceCall(
                filter: {{ node_did: {{ _eq: "{escaped_node}" }}, backend_id: {{ _eq: "{escaped_id}" }} }},
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
                cached_input_tokens cache_creation_input_tokens reasoning_output_tokens
                provider_total_tokens usage_completeness
            }}
        }}"#,
        limit = RECENT_CALLS_PER_BACKEND,
    );

    let resp = access
        .execute(&query)
        .await
        .with_context(|| format!("list InferenceCall for backend {backend_id}"))?;

    Ok(resp
        .get("data")
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
        cached_input_tokens: row.get("cached_input_tokens").and_then(|v| v.as_i64()),
        cache_creation_input_tokens: row
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64()),
        reasoning_output_tokens: row.get("reasoning_output_tokens").and_then(|v| v.as_i64()),
        provider_total_tokens: row.get("provider_total_tokens").and_then(|v| v.as_i64()),
        usage_completeness: row
            .get("usage_completeness")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
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
