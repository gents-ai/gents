use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use serde::{Deserialize, Serialize};

use super::protocol::{HttpRequestData, HttpResponse};
use crate::diagnostics::{
    build_desktop_client_snapshot, build_desktop_session_snapshot, build_request_diagnostics_bundle,
};
use crate::live_fixture::LiveBridgeFixture;
use gents_desktop_bridge::commands::mcp_health::{
    load_mcp_services_with_health, probe_mcp_service,
};
use gents_desktop_bridge::commands::{
    delete_event_source_config, delete_schedule_config, delete_tools_config, delete_trigger_config,
    rename_session, repair_p2p, run_schedule_config, run_task_config, save_agent_config,
    save_backend_config, save_event_source_config, save_inference_profile_config, save_node_config,
    save_schedule_config, save_task_config, save_tool_service_config, save_tools_config,
    save_trigger_config, send_chat_message, set_default_agent, test_tool_service_config,
};
use gents_desktop_bridge::interrupt::interrupt_request;
use gents_desktop_bridge::provenance::session_provenance_request;
use gents_desktop_bridge::snapshot::build_session_live_delta;
use gents_desktop_bridge::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use gents_desktop_bridge::tauri_commands::inference_setup::{
    discover_inference_models_for_core, inference_backend_recommendation,
    inference_model_recommendation, InferenceBackendRecommendationRequest,
    InferenceDiscoveryRequest, InferenceRecommendationRequest,
};
use gents_desktop_bridge::tauri_commands::operations::list_backends_with_health_for_core;
use gents_desktop_bridge::types::{
    AgentSaveRequest, BackendSaveRequest, ChatSendRequest, DefaultAgentSetRequest,
    DesktopInterruptRequest, DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopProbeMcpServiceRequest, DesktopSessionProvenanceRequest, EnrollmentRequestView,
    EnrollmentStatusRequest, EventSourceDeleteRequest, EventSourceSaveRequest,
    InferenceProfileSaveRequest, NativeExecutorStatusView, NodeConfigSaveRequest,
    PeerStatusFetchRequest, RuntimeLivenessView, ScheduleDeleteRequest, ScheduleRunRequest,
    ScheduleSaveRequest, SessionRenameRequest, TaskRunRequest, TaskSaveRequest,
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolsDeleteRequest, ToolsSaveRequest,
    TriggerDeleteRequest, TriggerSaveRequest,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSnapshotRequest {
    #[serde(default)]
    node_did: Option<String>,
    session_id: String,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionLiveDeltaRequest {
    #[serde(default)]
    node_did: Option<String>,
    session_id: String,
    request_id: String,
    base_live_cursor: String,
    base_content_byte_len: usize,
    base_content_hash: String,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectedNodeRequest {
    #[serde(default)]
    node_did: Option<String>,
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
            let server = fixture.remote_core();
            let identity: Arc<dyn gents::NodeIdentity> = Arc::new(server.node_identity().clone());
            let network =
                runtime.block_on(gents_desktop_bridge::enrollment::ensure_enrollment_network(
                    server.node(),
                    identity.as_ref(),
                    fixture.deployment_label(),
                ))?;
            let issuer = gents_desktop_bridge::enrollment::EnrollmentOfferIssuer::new(
                identity,
                Arc::clone(server.p2p()),
                network.network_id,
                fixture.node_did().to_string(),
                "client".to_string(),
            );
            let enrollment = runtime.block_on(issuer.mint())?;
            let schema = runtime.block_on(
                gents::agent::p2p_reconcile::read_client_replicated_schema(server.node_arc()),
            )?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({
                    "enrollment": enrollment,
                    (gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD): schema,
                    "node_name": deployment.label.clone(),
                    "node_did": deployment.node_did.clone(),
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
        ("GET", "/desktop/inference/setup/catalog") => Ok(HttpResponse::json_ok(
            serde_json::to_string(&gents::inference_setup::inference_setup_catalog())?,
        )),
        ("POST", "/desktop/inference/models/discover") => {
            let request = decode::<InferenceDiscoveryRequest>(
                &request.body,
                "decoding inference discovery request",
            )?;
            let result = runtime
                .block_on(discover_inference_models_for_core(
                    request,
                    Some(fixture.desktop_core().as_ref()),
                ))
                .map_err(|error| anyhow!("{error}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/inference/model/recommendation") => {
            let request = decode::<InferenceRecommendationRequest>(
                &request.body,
                "decoding inference recommendation request",
            )?;
            let result =
                inference_model_recommendation(request).map_err(|error| anyhow!("{error}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/inference/backend/recommendation") => {
            let request = decode::<InferenceBackendRecommendationRequest>(
                &request.body,
                "decoding backend recommendation request",
            )?;
            let result =
                inference_backend_recommendation(request).map_err(|error| anyhow!("{error}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
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
        ("POST", "/desktop/selected-node") => {
            let request = serde_json::from_str::<SelectedNodeRequest>(&request.body)
                .context("decoding selected node request")?;
            let did = request
                .node_did
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let core = fixture.desktop_core();
            core.set_selected_node_did(did.clone());
            if let Some(did) = did {
                let refreshed = match runtime.block_on(core.refresh_node(&did)) {
                    Ok(Some(_version)) => true,
                    Ok(None) => false,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            node_did = %did,
                            "local replica selection refresh failed"
                        );
                        false
                    }
                };
                if !refreshed {
                    runtime.block_on(core.ensure_node_loaded(&did))?;
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
                vec!["Network".to_string()],
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
            let enrollment =
                runtime.block_on(fixture.desktop_core().request_status_enrollment(&payload))?;
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
                request.node_did.as_deref(),
                &request.session_id,
                request.request_id.as_deref(),
                request.timeline_limit,
                request.timeline_before_item_key.as_deref(),
            ));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/session/live-delta") => {
            let request = serde_json::from_str::<SessionLiveDeltaRequest>(&request.body)
                .context("decoding session live delta request")?;
            let delta = runtime.block_on(build_session_live_delta(
                fixture.desktop_core().as_ref(),
                &request.session_id,
                request.node_did.as_deref(),
                &request.request_id,
                &request.base_live_cursor,
                request.base_content_byte_len,
                &request.base_content_hash,
                request.base_reasoning_byte_len,
                &request.base_reasoning_hash,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&delta)?))
        }
        ("POST", "/desktop/session/hydration/retry") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding session hydration retry request")?;
            let node_did = request.node_did.or_else(|| {
                fixture
                    .desktop_core()
                    .store()
                    .snapshot()
                    .sessions
                    .iter()
                    .find(|session| session.session_id == request.session_id)
                    .map(|session| session.node_did.clone())
            });
            let node_did = node_did
                .as_deref()
                .ok_or_else(|| anyhow!("session hydration retry requires a node"))?;
            runtime.block_on(
                fixture
                    .desktop_core()
                    .retry_session_hydration(&request.session_id, node_did),
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
        ("POST", "/desktop/request/retained-reasoning") => {
            let request = decode::<SessionSnapshotRequest>(
                &request.body,
                "decoding retained reasoning request",
            )?;
            let request_id = request
                .request_id
                .as_deref()
                .ok_or_else(|| anyhow!("requestId is required"))?;
            let result = runtime.block_on(retained_provider_reasoning(
                fixture.remote_core().as_ref(),
                fixture.node_did(),
                &request.session_id,
                request_id,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
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
        ("POST", "/desktop/session-provenance") => {
            let request = decode::<DesktopSessionProvenanceRequest>(
                &request.body,
                "decoding session provenance request",
            )?;
            let view = runtime
                .block_on(session_provenance_request(fixture.desktop_core(), request))
                .map_err(|e| anyhow!("{e}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&view)?))
        }
        ("GET", "/desktop/backend-health") => {
            let rows = runtime.block_on(list_backends_with_health_for_core(Arc::clone(
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
                &request.node_did,
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
        ("POST", "/desktop/queue/edit") => {
            let request = decode::<gents_desktop_bridge::types::PendingQueueEditRequest>(
                &request.body,
                "decoding pending queue edit",
            )?;
            let result = runtime.block_on(gents_desktop_bridge::commands::edit_pending_queue(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/session/rename") => {
            let request = decode::<SessionRenameRequest>(&request.body, "decoding rename request")?;
            runtime.block_on(rename_session(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "status": "ok" }).to_string(),
            ))
        }
        ("POST", "/desktop/node/save") => {
            let request = decode::<NodeConfigSaveRequest>(
                &request.body,
                "decoding node config save request",
            )?;
            runtime.block_on(save_node_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/node/default-agent") => {
            let request =
                decode::<DefaultAgentSetRequest>(&request.body, "decoding default agent request")?;
            runtime.block_on(set_default_agent(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/agent/save") => {
            let request = decode::<AgentSaveRequest>(&request.body, "decoding agent save request")?;
            runtime.block_on(save_agent_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        // Test-only escape hatch: write an agent document on the *remote* node so the
        // subsequent desktop-snapshot read exercises the real P2P propagation path (write
        // on remote core → visible on desktop core).  This is the D1/D2 cross-node
        // witness.  Only available when GENTS_TAURI_LIVE=1 (set by run-live-test.mjs).
        ("POST", "/desktop/test-fixture/remote-save-agent") => {
            if std::env::var("GENTS_TAURI_LIVE").as_deref() != Ok("1") {
                return Ok(HttpResponse::json_error(
                    "403 Forbidden",
                    "remote-save-agent is only available in live test mode (GENTS_TAURI_LIVE=1)",
                ));
            }
            let req =
                decode::<AgentSaveRequest>(&request.body, "decoding remote agent save request")?;
            tracing::info!(agent_id = %req.document.agent_id, "remote-save-agent: writing to remote core");
            runtime.block_on(save_agent_config(fixture.remote_core().as_ref(), req))?;
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
        ("POST", "/desktop/tools/save") => {
            let request =
                decode::<ToolsSaveRequest>(&request.body, "decoding tool selection save request")?;
            runtime.block_on(save_tools_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tools/delete") => {
            let request =
                decode::<ToolsDeleteRequest>(&request.body, "decoding tools delete request")?;
            runtime.block_on(delete_tools_config(
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
        ("POST", "/desktop/config/components/apply") => {
            let request = decode::<gents_desktop_bridge::types::ConfigComponentsApplyRequest>(
                &request.body,
                "decoding component apply request",
            )?;
            runtime.block_on(gents_desktop_bridge::commands::apply_config_components(
                fixture.remote_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/config/components/patch") => {
            let request = decode::<gents_desktop_bridge::types::ConfigComponentsPatchRequest>(
                &request.body,
                "decoding component patch request",
            )?;
            runtime.block_on(gents_desktop_bridge::commands::patch_config_components(
                fixture.remote_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
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
        ("POST", "/desktop/schedule/delete") => {
            let request =
                decode::<ScheduleDeleteRequest>(&request.body, "decoding schedule delete request")?;
            runtime.block_on(delete_schedule_config(
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
        ("POST", "/desktop/event-source/save") => {
            let request = decode::<EventSourceSaveRequest>(
                &request.body,
                "decoding event source save request",
            )?;
            runtime.block_on(save_event_source_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/event-source/delete") => {
            let request = decode::<EventSourceDeleteRequest>(
                &request.body,
                "decoding event source delete request",
            )?;
            runtime.block_on(delete_event_source_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/trigger/save") => {
            let request =
                decode::<TriggerSaveRequest>(&request.body, "decoding trigger save request")?;
            runtime.block_on(save_trigger_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/trigger/delete") => {
            let request =
                decode::<TriggerDeleteRequest>(&request.body, "decoding trigger delete request")?;
            runtime.block_on(delete_trigger_config(
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RetainedProviderReasoning {
    request_doc_id: String,
    reasoning_by_source: Vec<String>,
}

async fn retained_provider_reasoning(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
    request_id: &str,
) -> Result<RetainedProviderReasoning> {
    use gents::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::{
        reconstruction::{reconstruct_stream, ObservedSegment},
        OutputSource, PayloadRef, SourceClose, StreamPayload,
    };

    let node = gents::graphql::escape_graphql_string(node_did);
    let session = gents::graphql::escape_graphql_string(session_id);
    let logical = gents::graphql::escape_graphql_string(request_id);
    let request_query = format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{node}" }}, session_id: {{ _eq: "{session}" }}, request_id: {{ _eq: "{logical}" }} }}, limit: 2) {{ _docID }} }}"#
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        &core.node(),
        &request_query,
        "retained reasoning request lookup",
    )
    .await?;
    let requests = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .context("retained reasoning request lookup returned no rows")?;
    anyhow::ensure!(requests.len() == 1, "expected one exact physical request");
    let request_doc_id = requests[0]
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .context("request omitted physical _docID")?
        .to_owned();
    let physical = gents::graphql::escape_graphql_string(&request_doc_id);
    let segment_query = format!(
        r#"{{ AgentOutputSegment(filter: {{ node_did: {{ _eq: "{node}" }}, session_id: {{ _eq: "{session}" }}, request_doc_id: {{ _eq: "{physical}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        &core.node(),
        &segment_query,
        "retained reasoning segment lookup",
    )
    .await?;
    let values = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("retained reasoning segment lookup returned no rows")?;
    let rows = values
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    let mut sources = Vec::new();
    for row in &rows {
        if matches!(&row.segment.source, OutputSource::ProviderTurn { .. })
            && !row.segment.source.is_auxiliary_audit()
            && !sources.contains(&row.segment.source)
        {
            sources.push(row.segment.source.clone());
        }
    }
    let mut reasoning_by_source = Vec::new();
    for source in sources {
        let facts = rows
            .iter()
            .filter(|row| row.segment.source == source)
            .collect::<Vec<_>>();
        let closes = facts
            .iter()
            .filter(|row| row.segment.close.is_some())
            .collect::<Vec<_>>();
        anyhow::ensure!(closes.len() == 1, "provider source lacks one exact closure");
        let SourceClose::Closed { stream_bytes, .. } = closes[0].segment.close.as_ref().unwrap()
        else {
            continue;
        };
        let observed = facts
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let mut reasoning = String::new();
        for stream in 0..stream_bytes.len() {
            let reconstructed = reconstruct_stream(
                &observed,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: closes[0].doc_id.clone(),
                    stream: stream as u32,
                },
            )?;
            if matches!(
                reconstructed.declaration.payload,
                StreamPayload::Reasoning | StreamPayload::ReasoningSummary
            ) {
                reasoning.push_str(&reconstructed.text);
            }
        }
        reasoning_by_source.push(reasoning);
    }
    Ok(RetainedProviderReasoning {
        request_doc_id,
        reasoning_by_source,
    })
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
        node_did: request.node_did,
        liveness: Some(liveness),
        liveness_unavailable_reason: None,
        backgrounded_tools,
        stuck_diagnostics,
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
                stuck_since
            }
        }
    "#;

    let response =
        gents::graphql::graphql_with_transaction_retry(&core.node(), query, "AgentToolCall query")
            .await
            .map_err(|error| format!("{error:#}"))?;

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
            stuck_since: row
                .get("stuck_since")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        })
        .collect())
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
