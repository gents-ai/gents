use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use gents::backend_registry::{derive_display_state, list_all_backends};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::subagent_tree::{build_local_subagent_tree, effective_subagent_tree_max_depth};
use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use serde::{Deserialize, Serialize};

use super::protocol::{HttpRequestData, HttpResponse};
use crate::diagnostics::{
    build_desktop_client_snapshot, build_desktop_session_snapshot, build_request_diagnostics_bundle,
};
use crate::live_fixture::LiveBridgeFixture;
use gents_desktop_bridge::cascade::{build_cascade_preview, interrupt_request};
use gents_desktop_bridge::commands::mcp_health::{
    load_mcp_services_with_health, probe_mcp_service,
};
use gents_desktop_bridge::commands::{
    rename_conversation, repair_p2p, run_schedule_config, run_task_config, save_agent_config,
    save_backend_config, save_behavior_config, save_event_trigger_config,
    save_inference_profile_config, save_schedule_config, save_task_config,
    save_tool_selection_config, save_tool_service_config, send_chat_message,
    test_tool_service_config,
};
use gents_desktop_bridge::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use gents_desktop_bridge::tauri_commands::operations::{
    list_tool_call_holds_for_core, resolve_tool_call_hold_for_core, subagent_tree_view_from_gents,
};
use gents_desktop_bridge::types::{
    AgentConfigSaveRequest, BackendHealthView, BackendSaveRequest, BehaviorSaveRequest,
    ChatSendRequest, ConversationRenameRequest, DesktopInterruptRequest, DesktopListHoldsRequest,
    DesktopListSubagentTreeRequest, DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, DesktopProbeMcpServiceRequest,
    DesktopResolveHoldRequest, EnrollmentRequestView, EnrollmentStatusRequest,
    EventTriggerSaveRequest, InferenceCallSummaryView, InferenceProfileSaveRequest,
    NativeExecutorStatusView, PeerStatusFetchRequest, RuntimeLivenessView, ScheduleRunRequest,
    ScheduleSaveRequest, SubagentTreeView, TaskRunRequest, TaskSaveRequest,
    ToolSelectionSaveRequest, ToolServiceSaveRequest, ToolServiceTestRequest,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSnapshotRequest {
    #[serde(default)]
    agent_did: Option<String>,
    session_id: String,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectedAgentRequest {
    #[serde(default)]
    agent_did: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeerIdRequest {
    peer_id: String,
}

#[derive(Debug, Deserialize)]
struct ReplicatorRequest {
    #[serde(rename = "Collections")]
    collections: Vec<String>,
    #[serde(rename = "Addresses", default)]
    addresses: Vec<String>,
    #[serde(rename = "Filters", default)]
    filters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct DeleteReplicatorRequest {
    #[serde(rename = "Collections")]
    collections: Vec<String>,
    #[serde(rename = "ID")]
    id: String,
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
        ("OPTIONS", _) => Ok(HttpResponse::empty("204 No Content")),
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
                let refreshed = match runtime.block_on(core.refresh_agent(&did)) {
                    Ok(Some(_version)) => true,
                    Ok(None) => false,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            agent_did = %did,
                            "local replica selection refresh failed"
                        );
                        false
                    }
                };
                if !refreshed {
                    runtime.block_on(core.ensure_agent_loaded(&did))?;
                }
            }
            Ok(HttpResponse::json_ok(serde_json::json!({}).to_string()))
        }
        ("POST", "/desktop/test-fixture/remove-peer") => {
            require_live_fixture()?;
            let request = decode::<PeerIdRequest>(&request.body, "decoding peer removal")?;
            let removed =
                runtime.block_on(fixture.desktop_core().remove_peer(request.peer_id.trim()))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({
                    "peerId": removed.peer_id,
                    "label": removed.label,
                    "addr": removed.addr,
                    "connected": removed.connected,
                    "warning": removed.warning,
                })
                .to_string(),
            ))
        }
        ("POST", "/desktop/test-fixture/drift-remote-return-route") => {
            require_live_fixture()?;
            let desktop_address = runtime
                .block_on(fixture.desktop_core().p2p().shareable_address())?
                .context("desktop has no shareable address for route drift")?;
            runtime.block_on(fixture.remote_core().p2p().add_replicator(
                vec!["AgentNetwork".to_string()],
                Some(&desktop_address),
                Default::default(),
                Vec::new(),
                None,
            ))?;
            let replicators = runtime.block_on(fixture.remote_core().p2p().get_replicators())?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&replicators)?))
        }
        ("GET", "/desktop/test-fixture/remote-replicators") => {
            require_live_fixture()?;
            let replicators = runtime.block_on(fixture.remote_core().p2p().get_replicators())?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&replicators)?))
        }
        ("POST", "/desktop/peer/status") => {
            let request = decode::<PeerStatusFetchRequest>(
                &request.body,
                "decoding saved peer status request",
            )?;
            let address = runtime
                .block_on(fixture.desktop_core().peer_records())
                .into_iter()
                .find(|record| record.peer_id == request.peer_id)
                .map(|record| record.addr)
                .with_context(|| format!("saved peer {} was not found", request.peer_id))?;
            let payload = runtime.block_on(fetch_runtime_connection_payload(&address))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&payload)?))
        }
        ("POST", "/desktop/peer/enroll-status") => {
            let request = decode::<EnrollmentStatusRequest>(
                &request.body,
                "decoding enrollment status request",
            )?;
            let payload =
                runtime.block_on(fetch_runtime_connection_payload(&request.server_address))?;
            let token = payload
                .pointer("/enrollment/token")
                .and_then(serde_json::Value::as_str)
                .filter(|token| !token.trim().is_empty())
                .context("server does not advertise authenticated status enrollment")?;
            let enrollment =
                runtime.block_on(fixture.desktop_core().request_status_enrollment(token))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(
                &EnrollmentRequestView::from(enrollment),
            )?))
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
                request.timeline_limit,
                request.timeline_before_item_key.as_deref(),
            ));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/session/hydration/retry") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding session hydration retry request")?;
            let agent_did = request.agent_did.or_else(|| {
                fixture
                    .desktop_core()
                    .store()
                    .snapshot()
                    .conversations
                    .iter()
                    .find(|conversation| conversation.session_id == request.session_id)
                    .and_then(|conversation| conversation.agent_did.clone())
            });
            let agent_did = agent_did
                .as_deref()
                .ok_or_else(|| anyhow!("session hydration retry requires an agent"))?;
            runtime.block_on(
                fixture
                    .desktop_core()
                    .retry_session_hydration(&request.session_id, agent_did),
            )?;
            Ok(HttpResponse::json_ok("null".to_string()))
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
            let snapshot = runtime.block_on(operations_snapshot_response(
                Arc::clone(fixture.desktop_core()),
                request,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/subagent-tree") => {
            let request = decode::<DesktopListSubagentTreeRequest>(
                &request.body,
                "decoding subagent tree request",
            )?;
            let tree =
                runtime.block_on(list_subagent_tree_response(fixture.desktop_core(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&tree)?))
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
        ("POST", "/desktop/tool-call-holds/list") => {
            let request = decode::<DesktopListHoldsRequest>(
                &request.body,
                "decoding tool-call holds request",
            )?;
            let held = runtime.block_on(list_tool_call_holds_for_core(
                Arc::clone(fixture.desktop_core()),
                request,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&held)?))
        }
        ("POST", "/desktop/tool-call-holds/resolve") => {
            let request = decode::<DesktopResolveHoldRequest>(
                &request.body,
                "decoding tool-call hold resolution",
            )?;
            let result = runtime.block_on(resolve_tool_call_hold_for_core(
                Arc::clone(fixture.desktop_core()),
                request,
            ))?;
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
        // Test-only escape hatch: write a behavior document on the *remote* node so the
        // subsequent desktop-snapshot read exercises the real P2P propagation path (write
        // on remote core → visible on desktop core).  This is the D1/D2 cross-node
        // witness.  Only available when GENTS_TAURI_LIVE=1 (set by run-live-test.mjs).
        ("POST", "/desktop/test-fixture/remote-save-behavior") => {
            if std::env::var("GENTS_TAURI_LIVE").as_deref() != Ok("1") {
                return Ok(HttpResponse::json_error(
                    "403 Forbidden",
                    "remote-save-behavior is only available in live test mode (GENTS_TAURI_LIVE=1)",
                ));
            }
            let req = decode::<BehaviorSaveRequest>(
                &request.body,
                "decoding remote behavior save request",
            )?;
            tracing::info!(behavior_id = %req.behavior_id, "remote-save-behavior: writing to remote core");
            runtime.block_on(save_behavior_config(fixture.remote_core().as_ref(), req))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "ok": true }).to_string(),
            ))
        }
        ("POST", "/desktop/test-fixture/clear-client-store") => {
            if std::env::var("GENTS_TAURI_LIVE").as_deref() != Ok("1") {
                return Ok(HttpResponse::json_error(
                    "403 Forbidden",
                    "clear-client-store is only available in live test mode (GENTS_TAURI_LIVE=1)",
                ));
            }
            fixture
                .desktop_core()
                .store()
                .replace_snapshot(Default::default());
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "ok": true }).to_string(),
            ))
        }
        // Test-only projection of the remote node's P2P admin API. The managed
        // desktop route owner uses this through the same HTTP client and wire
        // shapes as a deployed runtime. Keeping it inside the live runner lets
        // the E2E provision two real nodes without a second server process.
        ("GET", "/p2p/info") => {
            require_live_fixture()?;
            let addresses = runtime.block_on(fixture.remote_core().p2p().listen_addresses())?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&addresses)?))
        }
        ("GET", "/p2p/active-peers") => {
            require_live_fixture()?;
            let peers = runtime.block_on(fixture.remote_core().p2p().connected_peers())?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&peers)?))
        }
        ("POST", "/p2p/connect") => {
            require_live_fixture()?;
            let addresses = decode::<Vec<String>>(&request.body, "decoding P2P connect")?;
            for address in addresses {
                runtime.block_on(fixture.remote_core().p2p().connect_peer(&address))?;
            }
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("GET", "/p2p/replicators") => {
            require_live_fixture()?;
            let replicators = runtime.block_on(fixture.remote_core().p2p().get_replicators())?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&replicators)?))
        }
        ("POST", "/p2p/replicators") => {
            require_live_fixture()?;
            let request =
                decode::<ReplicatorRequest>(&request.body, "decoding P2P replicator install")?;
            let filters = serde_json::from_value(request.filters)
                .context("decoding P2P replication filters")?;
            runtime.block_on(fixture.remote_core().p2p().add_replicator(
                request.collections,
                request.addresses.first().map(String::as_str),
                filters,
                Vec::new(),
                None,
            ))?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("DELETE", "/p2p/replicators") => {
            require_live_fixture()?;
            let request = decode::<DeleteReplicatorRequest>(
                &request.body,
                "decoding P2P replicator teardown",
            )?;
            runtime.block_on(
                fixture
                    .remote_core()
                    .p2p()
                    .remove_replicator(request.collections, Some(&request.id)),
            )?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("GET", "/p2p/collections") => {
            require_live_fixture()?;
            let collections = runtime.block_on(fixture.remote_core().p2p().get_collections())?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&collections)?))
        }
        ("POST", "/p2p/collections") => {
            require_live_fixture()?;
            let collections =
                decode::<Vec<String>>(&request.body, "decoding P2P collection install")?;
            runtime.block_on(fixture.remote_core().p2p().add_collections(collections))?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("DELETE", "/p2p/collections") => {
            require_live_fixture()?;
            let collections =
                decode::<Vec<String>>(&request.body, "decoding P2P collection teardown")?;
            runtime.block_on(fixture.remote_core().p2p().remove_collections(collections))?;
            Ok(HttpResponse::json_ok("{}".to_string()))
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

async fn operations_snapshot_response(
    core: Arc<ClientCore>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot> {
    let native_executors = gents::native_executor_status::active_native_executors()
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
    let tool_call_rows = fetch_background_tool_calls(&core)
        .await
        .map_err(|error| anyhow!("failed to query AgentToolCall: {error}"))?;
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
        agent_did: request.agent_did,
        liveness: Some(liveness),
        liveness_unavailable_reason: None,
        backgrounded_tools,
        stuck_diagnostics,
        lineage: None,
    })
}

async fn fetch_background_tool_calls(core: &Arc<ClientCore>) -> Result<Vec<ToolCallRow>, String> {
    let query = r#"
        query {
            AgentToolCall(
                filter: { await_mode: { _eq: "background" } }
            ) {
                request_id
                tool_call_id
                tool_name
                lifecycle_state
                status
                started_at
                deadline_at
                await_mode
                cancel_policy
                child_request_id
                stuck_since
                cancel_pending_remote_ack
            }
        }
    "#;

    let response = core.node().execute(query).await;
    if response.has_errors() {
        return Err(response
            .errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; "));
    }

    let data = response
        .data
        .ok_or_else(|| "AgentToolCall query returned no data".to_string())?;
    let rows = data
        .get("AgentToolCall")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            request_id: row
                .get("request_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            tool_call_id: row
                .get("tool_call_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            tool_name: row
                .get("tool_name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            lifecycle_state: row
                .get("lifecycle_state")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            status: row
                .get("status")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            started_at: row
                .get("started_at")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            deadline_at: row
                .get("deadline_at")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            await_mode: row
                .get("await_mode")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            cancel_policy: row
                .get("cancel_policy")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            child_request_id: row
                .get("child_request_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            stuck_since: row
                .get("stuck_since")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            cancel_pending_remote_ack: row
                .get("cancel_pending_remote_ack")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        })
        .collect())
}

async fn list_subagent_tree_response(
    core: &Arc<ClientCore>,
    request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView> {
    let root_request_id = request.root_request_id.trim();
    if root_request_id.is_empty() {
        anyhow::bail!("rootRequestId is required");
    }
    if request
        .agent_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        && core.selected_agent_did().is_none()
    {
        anyhow::bail!("no agent selected; pass agentDid explicitly");
    }
    let tree = build_local_subagent_tree(
        core.node_arc(),
        root_request_id,
        request.include_terminal.unwrap_or(false),
        effective_subagent_tree_max_depth(request.max_depth),
    )
    .await
    .context("local subagent tree query failed")?;
    Ok(subagent_tree_view_from_gents(tree))
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

fn require_live_fixture() -> Result<()> {
    if std::env::var("GENTS_TAURI_LIVE").as_deref() != Ok("1") {
        anyhow::bail!("test fixture route requires GENTS_TAURI_LIVE=1");
    }
    Ok(())
}

fn snapshot_response(
    runtime: &tokio::runtime::Handle,
    fixture: &Arc<LiveBridgeFixture>,
) -> Result<HttpResponse> {
    let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
    Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
}
