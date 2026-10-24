use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use gents_protocol::error_chain::ErrorChain;
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
    save_trigger_config, send_chat_message, session_rows, set_default_agent,
    test_tool_service_config,
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
    ScheduleSaveRequest, SessionRenameRequest, SessionRowsWindow, TaskRunRequest, TaskSaveRequest,
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
struct SessionRowsRequest {
    #[serde(default)]
    node_did: Option<String>,
    session_id: String,
    window: SessionRowsWindow,
    limit: Option<usize>,
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

#[derive(Debug, thiserror::Error)]
enum RouteError {
    #[error("{context}")]
    Decode {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("test fixture route requires GENTS_TAURI_LIVE=1")]
    FixtureDisabled,
    #[error("live bridge runner has no deployment")]
    NoDeployment,
    #[error("desktop has no shareable address for route drift")]
    NoShareableAddress,
    #[error("saved peer {peer_id} was not found")]
    SavedPeerNotFound { peer_id: String },
    #[error("session hydration retry requires a node")]
    HydrationNodeMissing,
    #[error("requestId is required")]
    RequestIdRequired,
    #[error("{message}")]
    DesktopCommand { message: String },
    #[error(transparent)]
    ConfigClient(#[from] gents::config_client::ConfigClientError),
    #[error(transparent)]
    CanonicalRows(#[from] gents::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    Reconstruction(#[from] gents_protocol::output::ReconstructionError),
    #[error("retained reasoning request lookup returned no rows")]
    RetainedRequestMissing,
    #[error("expected one exact physical request")]
    RetainedRequestNotExact,
    #[error("request omitted physical _docID")]
    RequestDocIdMissing,
    #[error("retained reasoning segment lookup returned no rows")]
    RetainedSegmentsMissing,
    #[error("provider source lacks one exact closure")]
    ProviderSourceClosure,
    #[error("AgentToolCall query returned no data")]
    AgentToolCallNoData,
    #[error("failed to query AgentToolCall: {}", ErrorChain(error.as_ref()))]
    AgentToolCallQuery { error: Box<RouteError> },
}

pub(super) async fn handle_request(
    fixture: &Arc<LiveBridgeFixture>,
    request: HttpRequestData,
) -> Result<HttpResponse> {
    match (request.method.as_str(), request.path.as_str()) {
        ("OPTIONS", _) => Ok(HttpResponse::empty("204 No Content")),
        ("GET", "/health") => Ok(HttpResponse::json_ok(
            serde_json::json!({ "status": "ok" }).to_string(),
        )),
        ("GET", "/status") => {
            let snapshot = build_desktop_client_snapshot(fixture).await;
            let deployment = snapshot
                .client
                .as_ref()
                .and_then(|client| client.deployments.first())
                .ok_or(RouteError::NoDeployment)?;
            let server = fixture.remote_core();
            let identity: Arc<dyn gents::NodeIdentity> = Arc::new(server.node_identity().clone());
            let network = gents_desktop_bridge::enrollment::ensure_enrollment_network(
                &server.node_arc(),
                identity.as_ref(),
                fixture.deployment_label(),
            )
            .await?;
            let issuer = gents_desktop_bridge::enrollment::EnrollmentOfferIssuer::new(
                identity,
                Arc::clone(server.p2p()),
                network.network_id,
                fixture.node_did().to_string(),
                "client".to_string(),
            );
            let enrollment = issuer.mint().await?;
            let schema =
                gents::agent::p2p_reconcile::read_client_replicated_schema(server.node_arc())
                    .await?;
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
            let snapshot = build_desktop_client_snapshot(fixture).await;
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
            let result =
                discover_inference_models_for_core(request, Some(fixture.desktop_core().as_ref()))
                    .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/inference/model/recommendation") => {
            let request = decode::<InferenceRecommendationRequest>(
                &request.body,
                "decoding inference recommendation request",
            )?;
            let result = inference_model_recommendation(request)?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/inference/backend/recommendation") => {
            let request = decode::<InferenceBackendRecommendationRequest>(
                &request.body,
                "decoding backend recommendation request",
            )?;
            let result = inference_backend_recommendation(request)?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/init") => Ok(HttpResponse::json_ok(serde_json::to_string(
            &fixture.init_summary(),
        )?)),
        ("POST", "/desktop/client/start") => {
            let snapshot = build_desktop_client_snapshot(fixture).await;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/client/shutdown") => Ok(HttpResponse::json_ok(
            serde_json::json!({
                "bootstrap": fixture.build_bootstrap_summary().await,
                "client": serde_json::Value::Null,
            })
            .to_string(),
        )),
        ("POST", "/desktop/selected-node") => {
            let request =
                decode::<SelectedNodeRequest>(&request.body, "decoding selected node request")?;
            let did = request
                .node_did
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let core = fixture.desktop_core();
            core.set_selected_node_did(did.clone());
            if let Some(did) = did {
                if let Err(error) = core.refresh_node(&did).await {
                    tracing::warn!(
                        error = %error,
                        node_did = %did,
                        "local replica selection refresh failed"
                    );
                }
            }
            Ok(HttpResponse::json_ok(serde_json::json!({}).to_string()))
        }
        ("POST", "/desktop/test-fixture/remove-peer") => {
            require_live_fixture()?;
            let request = decode::<PeerIdRequest>(&request.body, "decoding peer removal")?;
            let removed = fixture
                .desktop_core()
                .remove_peer(request.peer_id.trim())
                .await?;
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
            let desktop_address = fixture
                .desktop_core()
                .p2p()
                .shareable_address()
                .await?
                .ok_or(RouteError::NoShareableAddress)?;
            fixture
                .remote_core()
                .p2p()
                .add_replicator(
                    vec!["Network".to_string()],
                    Some(&desktop_address),
                    Default::default(),
                    Vec::new(),
                    None,
                )
                .await?;
            let replicators = fixture.remote_core().p2p().get_replicators().await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&replicators)?))
        }
        ("GET", "/desktop/test-fixture/remote-replicators") => {
            require_live_fixture()?;
            let replicators = fixture.remote_core().p2p().get_replicators().await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&replicators)?))
        }
        ("POST", "/desktop/peer/status") => {
            let request = decode::<PeerStatusFetchRequest>(
                &request.body,
                "decoding saved peer status request",
            )?;
            let address = fixture
                .desktop_core()
                .peer_records()
                .await
                .into_iter()
                .find(|record| record.peer_id == request.peer_id)
                .map(|record| record.addr)
                .ok_or_else(|| RouteError::SavedPeerNotFound {
                    peer_id: request.peer_id.clone(),
                })?;
            let payload = fetch_runtime_connection_payload(&address).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&payload)?))
        }
        ("POST", "/desktop/peer/enroll-status") => {
            let request = decode::<EnrollmentStatusRequest>(
                &request.body,
                "decoding enrollment status request",
            )?;
            let payload = fetch_runtime_connection_payload(&request.server_address).await?;
            let enrollment = fixture
                .desktop_core()
                .request_status_enrollment(&payload)
                .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(
                &EnrollmentRequestView::from(enrollment),
            )?))
        }
        ("POST", "/desktop/p2p/repair") => {
            repair_p2p(fixture.desktop_core().as_ref(), Duration::from_millis(250)).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/session/snapshot") => {
            let request = decode::<SessionSnapshotRequest>(
                &request.body,
                "decoding session snapshot request",
            )?;
            let snapshot = build_desktop_session_snapshot(
                fixture,
                request.node_did.as_deref(),
                &request.session_id,
                request.request_id.as_deref(),
                request.timeline_limit,
                request.timeline_before_item_key.as_deref(),
            )
            .await;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/session/rows") => {
            let request =
                decode::<SessionRowsRequest>(&request.body, "decoding session rows request")?;
            let rows = session_rows(
                fixture.desktop_core(),
                request.session_id,
                request.node_did,
                request.window,
                request.limit,
            )
            .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&rows)?))
        }
        ("POST", "/desktop/session/live-delta") => {
            let request = decode::<SessionLiveDeltaRequest>(
                &request.body,
                "decoding session live delta request",
            )?;
            let delta = build_session_live_delta(
                fixture.desktop_core().as_ref(),
                &request.session_id,
                request.node_did.as_deref(),
                &request.request_id,
                &request.base_live_cursor,
                request.base_content_byte_len,
                &request.base_content_hash,
                request.base_reasoning_byte_len,
                &request.base_reasoning_hash,
            )
            .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&delta)?))
        }
        ("POST", "/desktop/session/hydration/retry") => {
            let request = decode::<SessionSnapshotRequest>(
                &request.body,
                "decoding session hydration retry request",
            )?;
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
                .ok_or(RouteError::HydrationNodeMissing)?;
            fixture
                .desktop_core()
                .retry_session_hydration(&request.session_id, node_did)
                .await?;
            Ok(HttpResponse::json_ok("null".to_string()))
        }
        ("POST", "/desktop/request/diagnostics") => {
            let request = decode::<SessionSnapshotRequest>(
                &request.body,
                "decoding request diagnostics request",
            )?;
            let request_id = request
                .request_id
                .as_deref()
                .ok_or(RouteError::RequestIdRequired)?;
            let diagnostics =
                build_request_diagnostics_bundle(fixture, &request.session_id, request_id).await;
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
                .ok_or(RouteError::RequestIdRequired)?;
            let result = retained_provider_reasoning(
                fixture.remote_core().as_ref(),
                fixture.node_did(),
                &request.session_id,
                request_id,
            )
            .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/operations/snapshot") => {
            let request = decode::<DesktopOperationsSnapshotRequest>(
                &request.body,
                "decoding operations snapshot request",
            )?;
            let snapshot =
                operations_snapshot_response(Arc::clone(fixture.desktop_core()), request).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/session-provenance") => {
            let request = decode::<DesktopSessionProvenanceRequest>(
                &request.body,
                "decoding session provenance request",
            )?;
            let view = session_provenance_request(fixture.desktop_core(), request)
                .await
                .map_err(|message| RouteError::DesktopCommand { message })?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&view)?))
        }
        ("GET", "/desktop/backend-health") => {
            let rows =
                list_backends_with_health_for_core(Arc::clone(fixture.desktop_core())).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&rows)?))
        }
        ("GET", "/desktop/mcp-health") => {
            let rows = load_mcp_services_with_health(fixture.desktop_core().as_ref()).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&rows)?))
        }
        ("POST", "/desktop/mcp/probe") => {
            let request = decode::<DesktopProbeMcpServiceRequest>(
                &request.body,
                "decoding MCP probe request",
            )?;
            let result = probe_mcp_service(
                fixture.desktop_core().as_ref(),
                &request.node_did,
                &request.service_id,
            )
            .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/chat/send") => {
            let request = decode::<ChatSendRequest>(&request.body, "decoding chat send request")?;
            let result = send_chat_message(fixture.desktop_core().as_ref(), request).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/queue/edit") => {
            let request = decode::<gents_desktop_bridge::types::PendingQueueEditRequest>(
                &request.body,
                "decoding pending queue edit",
            )?;
            let result = gents_desktop_bridge::commands::edit_pending_queue(
                fixture.desktop_core().as_ref(),
                request,
            )
            .await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/session/rename") => {
            let request = decode::<SessionRenameRequest>(&request.body, "decoding rename request")?;
            rename_session(fixture.desktop_core().as_ref(), request).await?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "status": "ok" }).to_string(),
            ))
        }
        ("POST", "/desktop/node/save") => {
            let request = decode::<NodeConfigSaveRequest>(
                &request.body,
                "decoding node config save request",
            )?;
            save_node_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/node/default-agent") => {
            let request =
                decode::<DefaultAgentSetRequest>(&request.body, "decoding default agent request")?;
            set_default_agent(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/agent/save") => {
            let request = decode::<AgentSaveRequest>(&request.body, "decoding agent save request")?;
            save_agent_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        // Test-only: write on the remote core so the desktop-snapshot read
        // exercises the real P2P propagation path (cross-node witness).
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
            save_agent_config(fixture.remote_core().as_ref(), req).await?;
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
        // Test-only projection of the remote node's P2P admin API; must keep
        // the same wire shapes as a deployed runtime's HTTP client.
        ("GET", "/p2p/info") => {
            require_live_fixture()?;
            let addresses = fixture.remote_core().p2p().listen_addresses().await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&addresses)?))
        }
        ("GET", "/p2p/active-peers") => {
            require_live_fixture()?;
            let peers = fixture.remote_core().p2p().connected_peers().await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&peers)?))
        }
        ("POST", "/p2p/connect") => {
            require_live_fixture()?;
            let addresses = decode::<Vec<String>>(&request.body, "decoding P2P connect")?;
            for address in addresses {
                fixture.remote_core().p2p().connect_peer(&address).await?;
            }
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("GET", "/p2p/replicators") => {
            require_live_fixture()?;
            let replicators = fixture.remote_core().p2p().get_replicators().await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&replicators)?))
        }
        ("POST", "/p2p/replicators") => {
            require_live_fixture()?;
            let request =
                decode::<ReplicatorRequest>(&request.body, "decoding P2P replicator install")?;
            let filters =
                serde_json::from_value(request.filters).map_err(|source| RouteError::Decode {
                    context: "decoding P2P replication filters",
                    source,
                })?;
            fixture
                .remote_core()
                .p2p()
                .add_replicator(
                    request.collections,
                    request.addresses.first().map(String::as_str),
                    filters,
                    Vec::new(),
                    None,
                )
                .await?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("DELETE", "/p2p/replicators") => {
            require_live_fixture()?;
            let request = decode::<DeleteReplicatorRequest>(
                &request.body,
                "decoding P2P replicator teardown",
            )?;
            fixture
                .remote_core()
                .p2p()
                .remove_replicator(request.collections, Some(&request.id))
                .await?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("GET", "/p2p/collections") => {
            require_live_fixture()?;
            let collections = fixture.remote_core().p2p().get_collections().await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&collections)?))
        }
        ("POST", "/p2p/collections") => {
            require_live_fixture()?;
            let collections =
                decode::<Vec<String>>(&request.body, "decoding P2P collection install")?;
            fixture
                .remote_core()
                .p2p()
                .add_collections(collections)
                .await?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("DELETE", "/p2p/collections") => {
            require_live_fixture()?;
            let collections =
                decode::<Vec<String>>(&request.body, "decoding P2P collection teardown")?;
            fixture
                .remote_core()
                .p2p()
                .remove_collections(collections)
                .await?;
            Ok(HttpResponse::json_ok("{}".to_string()))
        }
        ("POST", "/desktop/backend/save") => {
            let request =
                decode::<BackendSaveRequest>(&request.body, "decoding backend save request")?;
            save_backend_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/inference-profile/save") => {
            let request = decode::<InferenceProfileSaveRequest>(
                &request.body,
                "decoding inference profile save request",
            )?;
            save_inference_profile_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/tools/save") => {
            let request =
                decode::<ToolsSaveRequest>(&request.body, "decoding tool selection save request")?;
            save_tools_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/tools/delete") => {
            let request =
                decode::<ToolsDeleteRequest>(&request.body, "decoding tools delete request")?;
            delete_tools_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/tool-service/save") => {
            let request = decode::<ToolServiceSaveRequest>(
                &request.body,
                "decoding tool service save request",
            )?;
            save_tool_service_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/tool-service/test") => {
            let request = decode::<ToolServiceTestRequest>(
                &request.body,
                "decoding tool service test request",
            )?;
            let result = test_tool_service_config(request).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/config/components/apply") => {
            let request = decode::<gents_desktop_bridge::types::ConfigComponentsApplyRequest>(
                &request.body,
                "decoding component apply request",
            )?;
            gents_desktop_bridge::commands::apply_config_components(
                fixture.remote_core().as_ref(),
                request,
            )
            .await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/config/components/patch") => {
            let request = decode::<gents_desktop_bridge::types::ConfigComponentsPatchRequest>(
                &request.body,
                "decoding component patch request",
            )?;
            gents_desktop_bridge::commands::patch_config_components(
                fixture.remote_core().as_ref(),
                request,
            )
            .await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/task/save") => {
            let request = decode::<TaskSaveRequest>(&request.body, "decoding task save request")?;
            save_task_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/schedule/save") => {
            let request =
                decode::<ScheduleSaveRequest>(&request.body, "decoding schedule save request")?;
            save_schedule_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/schedule/delete") => {
            let request =
                decode::<ScheduleDeleteRequest>(&request.body, "decoding schedule delete request")?;
            delete_schedule_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/schedule/run") => {
            let request =
                decode::<ScheduleRunRequest>(&request.body, "decoding schedule run request")?;
            let result = run_schedule_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/event-source/save") => {
            let request = decode::<EventSourceSaveRequest>(
                &request.body,
                "decoding event source save request",
            )?;
            save_event_source_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/event-source/delete") => {
            let request = decode::<EventSourceDeleteRequest>(
                &request.body,
                "decoding event source delete request",
            )?;
            delete_event_source_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/trigger/save") => {
            let request =
                decode::<TriggerSaveRequest>(&request.body, "decoding trigger save request")?;
            save_trigger_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/trigger/delete") => {
            let request =
                decode::<TriggerDeleteRequest>(&request.body, "decoding trigger delete request")?;
            delete_trigger_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(snapshot_response(fixture).await?)
        }
        ("POST", "/desktop/task/run") => {
            let request = decode::<TaskRunRequest>(&request.body, "decoding task run request")?;
            let result = run_task_config(fixture.desktop_core().as_ref(), request).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/tool/background") => {
            let background: gents_desktop_bridge::types::DesktopBackgroundToolRequest =
                decode(&request.body, "decoding background tool request")?;
            let core = fixture.desktop_core();
            let result =
                gents_desktop_bridge::commands::background_tool(core.as_ref(), background).await?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/interrupt/request") => {
            let request =
                decode::<DesktopInterruptRequest>(&request.body, "decoding interrupt request")?;
            let result = interrupt_request(fixture.desktop_core(), &request)
                .await
                .map_err(|message| RouteError::DesktopCommand { message })?;
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
) -> std::result::Result<RetainedProviderReasoning, RouteError> {
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
        core.node(),
        &request_query,
        "retained reasoning request lookup",
    )
    .await?;
    let requests = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .ok_or(RouteError::RetainedRequestMissing)?;
    if requests.len() != 1 {
        return Err(RouteError::RetainedRequestNotExact);
    }
    let request_doc_id = requests[0]
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .ok_or(RouteError::RequestDocIdMissing)?
        .to_owned();
    let physical = gents::graphql::escape_graphql_string(&request_doc_id);
    let segment_query = format!(
        r#"{{ AgentOutputSegment(filter: {{ node_did: {{ _eq: "{node}" }}, session_id: {{ _eq: "{session}" }}, request_doc_id: {{ _eq: "{physical}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        core.node(),
        &segment_query,
        "retained reasoning segment lookup",
    )
    .await?;
    let values = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .ok_or(RouteError::RetainedSegmentsMissing)?;
    let rows = values
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>, _>>()?;
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
        if closes.len() != 1 {
            return Err(RouteError::ProviderSourceClosure);
        }
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
) -> std::result::Result<DesktopOperationsSnapshot, RouteError> {
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
    let tool_call_rows = fetch_background_tool_calls(&core).await.map_err(|error| {
        RouteError::AgentToolCallQuery {
            error: Box::new(error),
        }
    })?;
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

async fn fetch_background_tool_calls(
    core: &Arc<ClientCore>,
) -> std::result::Result<Vec<ToolCallRow>, RouteError> {
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
                completed_at
                deadline_at
                await_mode
                stuck_since
                job_pin
            }
        }
    "#;

    let response =
        gents::graphql::graphql_with_transaction_retry(core.node(), query, "AgentToolCall query")
            .await?;

    let data = response.data.ok_or(RouteError::AgentToolCallNoData)?;
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
            completed_at: row
                .get("completed_at")
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
            job: row.get("job_pin").is_some_and(|pin| !pin.is_null()),
        })
        .collect())
}

fn decode<T>(body: &str, context: &'static str) -> Result<T, RouteError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str::<T>(body).map_err(|source| RouteError::Decode { context, source })
}

fn require_live_fixture() -> Result<(), RouteError> {
    if std::env::var("GENTS_TAURI_LIVE").as_deref() != Ok("1") {
        return Err(RouteError::FixtureDisabled);
    }
    Ok(())
}

async fn snapshot_response(fixture: &Arc<LiveBridgeFixture>) -> Result<HttpResponse, RouteError> {
    let snapshot = build_desktop_client_snapshot(fixture).await;
    Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
}
