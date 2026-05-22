use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use defra_agent::backend_registry::{derive_display_state, list_all_backends};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent_desktop_core::client::ClientCore;
use serde::{Deserialize, Serialize};

use super::protocol::{HttpRequestData, HttpResponse};
use crate::bridge::cascade::{build_cascade_preview, interrupt_request};
use crate::bridge::commands::mcp_health::{load_mcp_services_with_health, probe_mcp_service};
use crate::bridge::commands::{
    add_peer, rename_conversation, repair_p2p, run_schedule_config, run_task_config,
    save_agent_config, save_backend_config, save_behavior_config, save_event_trigger_config,
    save_inference_profile_config, save_schedule_config, save_task_config,
    save_tool_selection_config, save_tool_service_config, send_chat_message,
    test_tool_service_config,
};
use crate::bridge::types::{
    AgentConfigSaveRequest, BackendHealthView, BackendSaveRequest, BehaviorSaveRequest,
    ChatSendRequest, ConversationRenameRequest, DesktopInterruptRequest,
    DesktopListSubagentTreeRequest, DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, DesktopProbeMcpServiceRequest, EventTriggerSaveRequest,
    InferenceCallSummaryView, InferenceProfileSaveRequest, NativeExecutorStatusView,
    PeerAddRequest, RuntimeLivenessView, ScheduleRunRequest, ScheduleSaveRequest, SubagentTreeView,
    TaskRunRequest, TaskSaveRequest, ToolSelectionSaveRequest, ToolServiceSaveRequest,
    ToolServiceTestRequest,
};
use crate::diagnostics::{
    build_desktop_client_snapshot, build_desktop_session_snapshot, build_request_diagnostics_bundle,
};
use crate::live_fixture::LiveBridgeFixture;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSnapshotRequest {
    #[serde(default)]
    agent_did: Option<String>,
    session_id: String,
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectedAgentRequest {
    #[serde(default)]
    agent_did: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionResponse {
    version: u64,
}

const RECENT_CALLS_PER_BACKEND: usize = 10;

pub(super) fn handle_request(
    runtime: &tokio::runtime::Handle,
    fixture: &Arc<LiveBridgeFixture>,
    request: HttpRequestData,
) -> Result<HttpResponse> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => Ok(HttpResponse::json_ok(
            serde_json::json!({ "status": "ok" }).to_string(),
        )),
        ("GET", "/status") => {
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            let deployment = snapshot
                .client
                .as_ref()
                .and_then(|client| client.deployments.first())
                .ok_or_else(|| anyhow!("live bridge runner has no deployment"))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({
                    "agent_name": deployment.label.clone(),
                    "agent_did": deployment.agent_did.clone(),
                    "p2p_shareable_address": deployment.addr.clone(),
                    "p2p_listen_addresses": [deployment.addr.clone()],
                    "desktop_graphql": deployment.graphql.clone(),
                    "p2p": {
                        "p2p_shareable_address": deployment.addr.clone(),
                        "p2p_listen_addresses": [deployment.addr.clone()],
                    },
                })
                .to_string(),
            ))
        }
        ("GET", "/desktop/version") => Ok(HttpResponse::json_ok(serde_json::to_string(
            &VersionResponse {
                version: fixture.update_version(),
            },
        )?)),
        ("GET", "/desktop/client/snapshot") => {
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/init") => Ok(HttpResponse::json_ok(serde_json::to_string(
            &fixture.init_summary(),
        )?)),
        ("POST", "/desktop/client/start") => {
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/client/shutdown") => Ok(HttpResponse::json_ok(
            serde_json::json!({
                "bootstrap": runtime.block_on(fixture.build_bootstrap_summary()),
                "client": serde_json::Value::Null,
            })
            .to_string(),
        )),
        ("POST", "/desktop/selected-agent") => {
            let request = serde_json::from_str::<SelectedAgentRequest>(&request.body)
                .context("decoding selected agent request")?;
            let did = request
                .agent_did
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let core = fixture.desktop_core();
            core.set_selected_agent_did(did.clone());
            if let Some(did) = did {
                runtime.block_on(core.ensure_agent_loaded(&did))?;
            }
            Ok(HttpResponse::json_ok(serde_json::json!({}).to_string()))
        }
        ("POST", "/desktop/peer/add") => {
            let request = decode::<PeerAddRequest>(&request.body, "decoding peer add request")?;
            runtime.block_on(add_peer(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/p2p/repair") => {
            runtime.block_on(repair_p2p(
                fixture.desktop_core().as_ref(),
                Duration::from_millis(250),
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/session/snapshot") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding session snapshot request")?;
            let snapshot = runtime.block_on(build_desktop_session_snapshot(
                fixture,
                request.agent_did.as_deref(),
                &request.session_id,
                request.request_id.as_deref(),
            ));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/request/diagnostics") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding request diagnostics request")?;
            let request_id = request
                .request_id
                .as_deref()
                .ok_or_else(|| anyhow!("requestId is required"))?;
            let diagnostics = runtime.block_on(build_request_diagnostics_bundle(
                fixture,
                &request.session_id,
                request_id,
            ));
            Ok(HttpResponse::json_ok(serde_json::to_string(&diagnostics)?))
        }
        ("POST", "/desktop/operations/snapshot") => {
            let request = decode::<DesktopOperationsSnapshotRequest>(
                &request.body,
                "decoding operations snapshot request",
            )?;
            Ok(HttpResponse::json_ok(serde_json::to_string(
                &operations_snapshot_response(request),
            )?))
        }
        ("POST", "/desktop/subagent-tree") => {
            let request = decode::<DesktopListSubagentTreeRequest>(
                &request.body,
                "decoding subagent tree request",
            )?;
            Ok(HttpResponse::json_ok(serde_json::to_string(
                &SubagentTreeView {
                    root_request_id: request.root_request_id,
                    nodes: Vec::new(),
                    edges: Vec::new(),
                    truncated: false,
                },
            )?))
        }
        ("GET", "/desktop/backend-health") => {
            let rows = runtime.block_on(list_backends_with_health(Arc::clone(
                fixture.desktop_core(),
            )))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&rows)?))
        }
        ("GET", "/desktop/mcp-health") => {
            let rows = runtime.block_on(load_mcp_services_with_health(
                fixture.desktop_core().as_ref(),
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&rows)?))
        }
        ("POST", "/desktop/mcp/probe") => {
            let request = decode::<DesktopProbeMcpServiceRequest>(
                &request.body,
                "decoding MCP probe request",
            )?;
            let result = runtime.block_on(probe_mcp_service(
                fixture.desktop_core().as_ref(),
                &request.service_id,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/chat/send") => {
            let request = decode::<ChatSendRequest>(&request.body, "decoding chat send request")?;
            let result =
                runtime.block_on(send_chat_message(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/conversation/rename") => {
            let request =
                decode::<ConversationRenameRequest>(&request.body, "decoding rename request")?;
            runtime.block_on(rename_conversation(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "status": "ok" }).to_string(),
            ))
        }
        ("POST", "/desktop/agent/save") => {
            let request = decode::<AgentConfigSaveRequest>(
                &request.body,
                "decoding agent config save request",
            )?;
            runtime.block_on(save_agent_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/behavior/save") => {
            let request =
                decode::<BehaviorSaveRequest>(&request.body, "decoding behavior save request")?;
            runtime.block_on(save_behavior_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/backend/save") => {
            let request =
                decode::<BackendSaveRequest>(&request.body, "decoding backend save request")?;
            runtime.block_on(save_backend_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/inference-profile/save") => {
            let request = decode::<InferenceProfileSaveRequest>(
                &request.body,
                "decoding inference profile save request",
            )?;
            runtime.block_on(save_inference_profile_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tool-selection/save") => {
            let request = decode::<ToolSelectionSaveRequest>(
                &request.body,
                "decoding tool selection save request",
            )?;
            runtime.block_on(save_tool_selection_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tool-service/save") => {
            let request = decode::<ToolServiceSaveRequest>(
                &request.body,
                "decoding tool service save request",
            )?;
            runtime.block_on(save_tool_service_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tool-service/test") => {
            let request = decode::<ToolServiceTestRequest>(
                &request.body,
                "decoding tool service test request",
            )?;
            let result = runtime.block_on(test_tool_service_config(request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/task/save") => {
            let request = decode::<TaskSaveRequest>(&request.body, "decoding task save request")?;
            runtime.block_on(save_task_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/schedule/save") => {
            let request =
                decode::<ScheduleSaveRequest>(&request.body, "decoding schedule save request")?;
            runtime.block_on(save_schedule_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/schedule/run") => {
            let request =
                decode::<ScheduleRunRequest>(&request.body, "decoding schedule run request")?;
            let result = runtime.block_on(run_schedule_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/event-trigger/save") => {
            let request = decode::<EventTriggerSaveRequest>(
                &request.body,
                "decoding event trigger save request",
            )?;
            runtime.block_on(save_event_trigger_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/task/run") => {
            let request = decode::<TaskRunRequest>(&request.body, "decoding task run request")?;
            let result =
                runtime.block_on(run_task_config(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/interrupt/preview") => {
            let request = decode::<DesktopPreviewInterruptCascadeRequest>(
                &request.body,
                "decoding interrupt preview request",
            )?;
            let result = runtime
                .block_on(build_cascade_preview(fixture.desktop_core(), &request))
                .map_err(|e| anyhow!("{e}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/interrupt/request") => {
            let request =
                decode::<DesktopInterruptRequest>(&request.body, "decoding interrupt request")?;
            let result = runtime
                .block_on(interrupt_request(fixture.desktop_core(), &request))
                .map_err(|e| anyhow!("{e}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        _ => Ok(HttpResponse::json_error("404 Not Found", "not found")),
    }
}

fn operations_snapshot_response(
    request: DesktopOperationsSnapshotRequest,
) -> DesktopOperationsSnapshot {
    let native_executors = defra_agent::native_executor_status::active_native_executors()
        .into_iter()
        .map(|executor| NativeExecutorStatusView {
            id: executor.id as i64,
            pid: executor.pid as u32,
            argv0: executor.argv0,
            tool_name: executor.tool_name,
            started_at: executor.started_at,
            age_ms: executor.age_ms,
        })
        .collect();
    DesktopOperationsSnapshot {
        fetched_at: Utc::now().to_rfc3339(),
        agent_did: request.agent_did,
        liveness: Some(RuntimeLivenessView {
            expired_processing_count: 0,
            requests: Vec::new(),
            active_tool_calls: Vec::new(),
            active_native_executors_available: true,
            active_native_executors: native_executors,
        }),
        liveness_unavailable_reason: None,
        backgrounded_tools: Vec::new(),
        stuck_diagnostics: Vec::new(),
        lineage: None,
    }
}

async fn list_backends_with_health(core: Arc<ClientCore>) -> Result<Vec<BackendHealthView>> {
    let node = core.node();
    let backends = list_all_backends(node).await?;
    let mut views = Vec::with_capacity(backends.len());
    for backend in backends {
        let recent_calls = fetch_recent_calls(node, &backend.backend_id).await?;
        views.push(BackendHealthView {
            backend_id: backend.backend_id,
            name: backend.name,
            provider_kind: backend.provider_kind.as_str().to_string(),
            endpoint: backend.endpoint,
            enabled: backend.enabled,
            probe_status: backend.probe_status.clone(),
            display_state: derive_display_state(backend.enabled, &backend.probe_status).to_string(),
            last_probe: None,
            max_concurrent: backend.max_concurrent,
            max_queue_depth: backend.max_queue_depth,
            models: backend.models,
            recent_calls,
        });
    }
    Ok(views)
}

async fn fetch_recent_calls(
    node: &EmbeddedNode,
    backend_id: &str,
) -> Result<Vec<InferenceCallSummaryView>> {
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

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "list InferenceCall for backend {backend_id} failed: {:?}",
            response.errors
        );
    }

    Ok(response
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
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        call_seq: row
            .get("call_seq")
            .and_then(|value| value.as_i64())
            .unwrap_or(0),
        call_kind: row
            .get("call_kind")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        call_state: row
            .get("call_state")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        failure_reason: row
            .get("failure_reason")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        queued_at: row
            .get("queued_at")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        started_at: row
            .get("started_at")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        ended_at: row
            .get("ended_at")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
        queue_depth_at_enqueue: row
            .get("queue_depth_at_enqueue")
            .and_then(|value| value.as_i64()),
        prompt_tokens: row.get("prompt_tokens").and_then(|value| value.as_i64()),
        completion_tokens: row
            .get("completion_tokens")
            .and_then(|value| value.as_i64()),
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: &str, context: &str) -> Result<T> {
    serde_json::from_str::<T>(body).context(context.to_string())
}

fn snapshot_response(
    runtime: &tokio::runtime::Handle,
    fixture: &Arc<LiveBridgeFixture>,
) -> Result<HttpResponse> {
    let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
    Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
}
