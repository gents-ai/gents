#[path = "../runner/live_fixture.rs"]
mod live_fixture;

mod bridge {
    #[allow(dead_code)]
    pub mod types {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bridge/types.rs"));
    }
    #[allow(dead_code)]
    pub mod snapshot {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/bridge/snapshot.rs"
        ));
    }
}

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::mcp_pool::{resolve_mcp_url, McpPool};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, AgentRequestRow, EventTriggerRow, InferenceBackendRow,
    InferenceProfileRow, ScheduleRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use serde::Deserialize;
use serde::Serialize;

use bridge::snapshot::{build_runtime_snapshot, build_session_snapshot_from_store};
use bridge::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, ChatSendRequest,
    ChatSendResult, ConversationRenameRequest, DesktopClientSnapshot, DesktopSessionSnapshot,
    EventTriggerSaveRequest, InferenceProfileSaveRequest, PeerAddRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest, ToolSelectionSaveRequest,
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolServiceTestResult, ToolServiceToolView,
};
use live_fixture::{can_send_in_turn, turn_state_label, LiveBackendOverride, LiveBridgeFixture};

#[derive(Debug, Parser)]
struct RunnerArgs {
    #[arg(long)]
    inference_url: Option<String>,
    #[arg(long)]
    model_name: Option<String>,
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    api_key: Option<String>,
    #[arg(long)]
    api_key_env_var: Option<String>,
}

#[derive(Debug)]
struct HttpRequestData {
    method: String,
    path: String,
    body: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSnapshotRequest {
    session_id: String,
    request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestRowDiagnostics {
    status: Option<String>,
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
    created_at: Option<String>,
    claimed_at: Option<String>,
    interrupt_requested_at: Option<String>,
    valid_until: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseRowDiagnostics {
    status: Option<String>,
    error_message: Option<String>,
    progress_seq: Option<i64>,
    materialized_message_sequence: Option<i64>,
    materialized_at: Option<String>,
    completed_at: Option<String>,
    content_len: usize,
    reasoning_len: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallDiagnostics {
    total: usize,
    completed: usize,
    pending: usize,
    latest_tool_name: Option<String>,
    latest_status: Option<String>,
    latest_completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestDiagnostics {
    source: String,
    session_id: String,
    request_id: String,
    refresh_error: Option<String>,
    turn_state: Option<String>,
    latest_request_id: Option<String>,
    conversation_updated_at: Option<String>,
    request: Option<RequestRowDiagnostics>,
    response: Option<ResponseRowDiagnostics>,
    matching_response_count: usize,
    matching_response_progress_seqs: Vec<i64>,
    matching_response_statuses: Vec<String>,
    tool_calls: ToolCallDiagnostics,
    tool_result_count: usize,
    message_count: usize,
    timeline_count: usize,
    active_response_overlay_content_len: usize,
    active_response_overlay_reasoning_len: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestDiagnosticsBundle {
    desktop: RequestDiagnostics,
    remote: RequestDiagnostics,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionResponse {
    version: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyMessage {
    kind: &'static str,
    base_url: String,
    deployment_label: String,
    agent_did: String,
    tool_root: String,
}

struct BridgeRunnerServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BridgeRunnerServer {
    fn start(fixture: Arc<LiveBridgeFixture>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let runtime = fixture.runtime().handle().clone();
        let fixture_for_thread = Arc::clone(&fixture);
        let thread = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let response = match read_http_request(&mut stream).and_then(|request| {
                            handle_request(&runtime, &fixture_for_thread, request)
                        }) {
                            Ok(response) => response,
                            Err(error) => HttpResponse::json_error(
                                "500 Internal Server Error",
                                &error.to_string(),
                            ),
                        };
                        let _ = write_http_response(
                            &mut stream,
                            response.status,
                            response.content_type,
                            &response.body,
                        );
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            port,
            stop,
            handle: Some(thread),
        })
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct HttpResponse {
    status: &'static str,
    content_type: &'static str,
    body: String,
}

impl HttpResponse {
    fn json_ok(body: String) -> Self {
        Self {
            status: "200 OK",
            content_type: "application/json",
            body,
        }
    }

    fn json_error(status: &'static str, error: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::json!({ "error": error }).to_string(),
        }
    }
}

fn main() -> Result<()> {
    let args = RunnerArgs::parse();
    let backend_override = LiveBackendOverride {
        inference_url: args.inference_url,
        model_name: args.model_name,
        provider: args.provider,
        api_key: args.api_key,
        api_key_env_var: args.api_key_env_var,
    };
    let fixture = std::panic::catch_unwind(|| LiveBridgeFixture::start(Some(backend_override)))
        .map_err(|_| anyhow!("bridge runner panicked during startup"))??;
    let server = BridgeRunnerServer::start(Arc::clone(&fixture))?;
    let ready = ReadyMessage {
        kind: "ready",
        base_url: server.base_url(),
        deployment_label: fixture.deployment_label().to_string(),
        agent_did: fixture.agent_did().to_string(),
        tool_root: fixture.tool_root().display().to_string(),
    };
    println!("{}", serde_json::to_string(&ready)?);
    std::io::stdout().flush().ok();

    let result = wait_for_shutdown_signal();
    server.stop();
    fixture.runtime().block_on(fixture.shutdown())?;
    result
}

fn wait_for_shutdown_signal() -> Result<()> {
    let mut stdin = std::io::stdin();
    let mut buffer = [0_u8; 1];
    loop {
        match stdin.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading bridge runner stdin"),
        }
    }
}

fn handle_request(
    runtime: &tokio::runtime::Handle,
    fixture: &Arc<LiveBridgeFixture>,
    request: HttpRequestData,
) -> Result<HttpResponse> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => Ok(HttpResponse::json_ok(
            serde_json::json!({ "status": "ok" }).to_string(),
        )),
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
        ("POST", "/desktop/peer/add") => {
            let request = serde_json::from_str::<PeerAddRequest>(&request.body)
                .context("decoding peer add request")?;
            runtime.block_on(async {
                fixture
                    .desktop_core()
                    .add_peer(&request.label, &request.addr, &request.agent_did)
                    .await
            })?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/p2p/repair") => {
            runtime.block_on(async {
                fixture.desktop_core().request_p2p_repair().await?;
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok::<(), anyhow::Error>(())
            })?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/session/snapshot") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding session snapshot request")?;
            let snapshot = runtime.block_on(build_desktop_session_snapshot(
                fixture,
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
        ("POST", "/desktop/chat/send") => {
            let request = serde_json::from_str::<ChatSendRequest>(&request.body)
                .context("decoding chat send request")?;
            let result = runtime.block_on(send_chat_message(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/conversation/rename") => {
            let request = serde_json::from_str::<ConversationRenameRequest>(&request.body)
                .context("decoding rename request")?;
            runtime.block_on(rename_conversation(fixture, request))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "status": "ok" }).to_string(),
            ))
        }
        ("POST", "/desktop/agent/save") => {
            let request = serde_json::from_str::<AgentConfigSaveRequest>(&request.body)
                .context("decoding agent config save request")?;
            let snapshot = runtime.block_on(save_agent_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/behavior/save") => {
            let request = serde_json::from_str::<BehaviorSaveRequest>(&request.body)
                .context("decoding behavior save request")?;
            let snapshot = runtime.block_on(save_behavior_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/backend/save") => {
            let request = serde_json::from_str::<BackendSaveRequest>(&request.body)
                .context("decoding backend save request")?;
            let snapshot = runtime.block_on(save_backend_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/inference-profile/save") => {
            let request = serde_json::from_str::<InferenceProfileSaveRequest>(&request.body)
                .context("decoding inference profile save request")?;
            let snapshot = runtime.block_on(save_inference_profile_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/tool-selection/save") => {
            let request = serde_json::from_str::<ToolSelectionSaveRequest>(&request.body)
                .context("decoding tool selection save request")?;
            let snapshot = runtime.block_on(save_tool_selection_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/tool-service/save") => {
            let request = serde_json::from_str::<ToolServiceSaveRequest>(&request.body)
                .context("decoding tool service save request")?;
            let snapshot = runtime.block_on(save_tool_service_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/tool-service/test") => {
            let request = serde_json::from_str::<ToolServiceTestRequest>(&request.body)
                .context("decoding tool service test request")?;
            let result = runtime.block_on(test_tool_service_config(request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/task/save") => {
            let request = serde_json::from_str::<TaskSaveRequest>(&request.body)
                .context("decoding task save request")?;
            let snapshot = runtime.block_on(save_task_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/schedule/save") => {
            let request = serde_json::from_str::<ScheduleSaveRequest>(&request.body)
                .context("decoding schedule save request")?;
            let snapshot = runtime.block_on(save_schedule_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/schedule/run") => {
            let request = serde_json::from_str::<ScheduleRunRequest>(&request.body)
                .context("decoding schedule run request")?;
            let result = runtime.block_on(run_schedule_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/event-trigger/save") => {
            let request = serde_json::from_str::<EventTriggerSaveRequest>(&request.body)
                .context("decoding event trigger save request")?;
            let snapshot = runtime.block_on(save_event_trigger_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/task/run") => {
            let request = serde_json::from_str::<TaskRunRequest>(&request.body)
                .context("decoding task run request")?;
            let result = runtime.block_on(run_task_config(fixture, request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        _ => Ok(HttpResponse::json_error("404 Not Found", "not found")),
    }
}

async fn build_desktop_client_snapshot(fixture: &LiveBridgeFixture) -> DesktopClientSnapshot {
    let _ = refresh_store_with_timeout(fixture.desktop_core().as_ref()).await;
    DesktopClientSnapshot {
        bootstrap: fixture.build_bootstrap_summary().await,
        client: Some(build_runtime_snapshot(fixture.desktop_core().as_ref()).await),
    }
}

async fn build_desktop_session_snapshot(
    fixture: &LiveBridgeFixture,
    session_id: &str,
    request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let _ = refresh_store_with_timeout(fixture.desktop_core().as_ref()).await;
    let snapshot = fixture.desktop_core().store().snapshot();
    build_session_snapshot_from_store(snapshot.as_ref(), session_id, request_id)
}

async fn build_request_diagnostics_bundle(
    fixture: &LiveBridgeFixture,
    session_id: &str,
    request_id: &str,
) -> RequestDiagnosticsBundle {
    RequestDiagnosticsBundle {
        desktop: build_request_diagnostics(
            "desktop",
            fixture.desktop_core().as_ref(),
            session_id,
            request_id,
        )
        .await,
        remote: build_request_diagnostics(
            "remote",
            fixture.remote_core().as_ref(),
            session_id,
            request_id,
        )
        .await,
    }
}

async fn build_request_diagnostics(
    source: &str,
    core: &defra_agent_desktop_core::client::ClientCore,
    session_id: &str,
    request_id: &str,
) -> RequestDiagnostics {
    let refresh_error = refresh_store_with_timeout(core).await;
    let snapshot = core.store().snapshot();
    let request = snapshot
        .requests
        .iter()
        .find(|row| row.request_id == request_id)
        .cloned();
    let matching_responses = snapshot
        .responses
        .iter()
        .filter(|row| row.request_id.as_deref() == Some(request_id))
        .collect::<Vec<_>>();
    let response = snapshot.latest_response_for_request(request_id).cloned();
    let transcript = snapshot.transcript(session_id);
    let relevant_tool_calls = transcript.tool_calls;
    let latest_tool_call = relevant_tool_calls.last().copied();
    let completed_tool_calls = relevant_tool_calls
        .iter()
        .filter(|row| {
            row.completed_at
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || row
                    .status
                    .as_deref()
                    .is_some_and(|value| matches!(value, "completed" | "success" | "ok"))
        })
        .count();
    let session_snapshot =
        build_session_snapshot_from_store(snapshot.as_ref(), session_id, Some(request_id));

    RequestDiagnostics {
        source: source.to_string(),
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        refresh_error,
        turn_state: snapshot
            .derive_turn_for_request(request_id)
            .or_else(|| snapshot.derive_turn(session_id))
            .map(turn_state_label)
            .map(str::to_string),
        latest_request_id: snapshot.latest_request_id_for_session(session_id),
        conversation_updated_at: snapshot
            .conversations
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| row.updated_at.clone()),
        request: request.map(|row| RequestRowDiagnostics {
            status: row.status.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            failure_reason: row.failure_reason.clone(),
            created_at: row.created_at.clone(),
            claimed_at: row.claimed_at.clone(),
            interrupt_requested_at: row.interrupt_requested_at.clone(),
            valid_until: row.valid_until.clone(),
        }),
        response: response.map(|row| ResponseRowDiagnostics {
            status: row.status.clone(),
            error_message: row.error_message.clone(),
            progress_seq: row.progress_seq,
            materialized_message_sequence: row.materialized_message_sequence,
            materialized_at: row.materialized_at.clone(),
            completed_at: row.completed_at.clone(),
            content_len: row.content.as_deref().map_or(0, str::len),
            reasoning_len: row.reasoning.as_deref().map_or(0, str::len),
        }),
        matching_response_count: matching_responses.len(),
        matching_response_progress_seqs: matching_responses
            .iter()
            .map(|row| row.progress_seq.unwrap_or_default())
            .collect(),
        matching_response_statuses: matching_responses
            .iter()
            .map(|row| row.status.clone().unwrap_or_default())
            .collect(),
        tool_calls: ToolCallDiagnostics {
            total: relevant_tool_calls.len(),
            completed: completed_tool_calls,
            pending: relevant_tool_calls
                .len()
                .saturating_sub(completed_tool_calls),
            latest_tool_name: latest_tool_call.and_then(|row| row.tool_name.clone()),
            latest_status: latest_tool_call.and_then(|row| row.status.clone()),
            latest_completed_at: latest_tool_call.and_then(|row| row.completed_at.clone()),
        },
        tool_result_count: transcript.tool_results.len(),
        message_count: transcript.messages.len(),
        timeline_count: session_snapshot
            .as_ref()
            .map_or(0, |session| session.timeline_items.len()),
        active_response_overlay_content_len: session_snapshot
            .as_ref()
            .and_then(|session| session.active_response_overlay.as_ref())
            .and_then(|overlay| overlay.content.as_deref())
            .map_or(0, str::len),
        active_response_overlay_reasoning_len: session_snapshot
            .as_ref()
            .and_then(|session| session.active_response_overlay.as_ref())
            .and_then(|overlay| overlay.reasoning.as_deref())
            .map_or(0, str::len),
    }
}

async fn refresh_store_with_timeout(
    core: &defra_agent_desktop_core::client::ClientCore,
) -> Option<String> {
    match tokio::time::timeout(Duration::from_secs(5), core.refresh_store()).await {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("timed out refreshing store".to_string()),
    }
}

fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn require_trimmed(name: &str, value: impl AsRef<str>) -> Result<String> {
    let value = value.as_ref().trim().to_string();
    if value.is_empty() {
        anyhow::bail!("{name} is required");
    }
    Ok(value)
}

fn validate_event_kind(event_kind: &str) -> Result<()> {
    if event_kind == "created" {
        Ok(())
    } else {
        anyhow::bail!("event_kind currently supports only created")
    }
}

fn resolve_tool_service_endpoint(request: &ToolServiceTestRequest) -> Result<String> {
    let mcp_port = request
        .mcp_port
        .ok_or_else(|| anyhow!("mcp_port is required"))?;
    if !(1..=u16::MAX as i64).contains(&mcp_port) {
        anyhow::bail!("mcp_port must be between 1 and 65535");
    }
    let hostname = trim_optional(request.hostname.clone()).unwrap_or_default();
    let tailscale_ip = trim_optional(request.tailscale_ip.clone()).unwrap_or_default();
    let lan_ip = trim_optional(request.lan_ip.clone()).unwrap_or_default();
    if hostname.is_empty() && tailscale_ip.is_empty() && lan_ip.is_empty() {
        anyhow::bail!("hostname, tailscale_ip, or lan_ip is required");
    }
    Ok(resolve_mcp_url(
        &hostname,
        &tailscale_ip,
        &lan_ip,
        mcp_port as u16,
        request.mcp_path.as_deref().unwrap_or("/mcp"),
        "",
        None,
    ))
}

async fn load_agent_request_by_doc_id(
    core: &defra_agent_desktop_core::client::ClientCore,
    request_doc_id: &str,
) -> Result<AgentRequestRow> {
    let escaped_doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                request_id
                agent_did
                behavior_id
                session_id
                status
                lifecycle_state
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query manual task run request failed: {:?}",
            response.errors
        );
    }

    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow!("manual task run request {request_doc_id} was not found"))?;
    serde_json::from_value(row).map_err(Into::into)
}

async fn send_chat_message(
    fixture: &LiveBridgeFixture,
    request: ChatSendRequest,
) -> Result<ChatSendResult> {
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        anyhow::bail!("agent_did is required");
    }

    let content = request.content.trim().to_string();
    if content.is_empty() {
        anyhow::bail!("content is required");
    }

    let behavior_id = request
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let session_id = match request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(session_id) => session_id.to_string(),
        None => {
            fixture
                .desktop_core()
                .create_conversation(&agent_did, behavior_id.as_deref())
                .await?
                .session_id
        }
    };

    let store = fixture.desktop_core().store().snapshot();
    if let Some(turn_state) = store.derive_turn(&session_id) {
        if !can_send_in_turn(turn_state) {
            anyhow::bail!(
                "cannot send while current turn is {}",
                turn_state_label(turn_state)
            );
        }
    }

    let submitted = fixture
        .desktop_core()
        .submit_request(&session_id, &agent_did, &content, behavior_id.as_deref())
        .await?;

    Ok(ChatSendResult {
        session_id,
        request_id: submitted.request_id,
        agent_did: submitted.agent_did,
        behavior_id: submitted.behavior_id,
    })
}

async fn rename_conversation(
    fixture: &LiveBridgeFixture,
    request: ConversationRenameRequest,
) -> Result<()> {
    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        anyhow::bail!("session_id is required");
    }
    let title = request.title.trim().to_string();
    if title.is_empty() {
        anyhow::bail!("title is required");
    }
    fixture
        .desktop_core()
        .rename_conversation(&session_id, &title)
        .await?;
    Ok(())
}

async fn save_agent_config(
    fixture: &LiveBridgeFixture,
    request: AgentConfigSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let display_name = require_trimmed("display_name", request.display_name)?;
    let default_behavior_id = require_trimmed("default_behavior_id", request.default_behavior_id)?;

    let store = fixture.desktop_core().store().snapshot();
    if !store.behaviors.iter().any(|behavior| {
        behavior.agent_did.as_deref() == Some(agent_did.as_str())
            && behavior.behavior_id == default_behavior_id
    }) {
        anyhow::bail!("default_behavior_id {default_behavior_id} does not exist for {agent_did}");
    }

    let mut row = store
        .agent_principals
        .iter()
        .find(|row| row.agent_did == agent_did)
        .cloned()
        .unwrap_or_else(|| AgentPrincipalRow {
            agent_did: agent_did.clone(),
            display_name: None,
            default_behavior_id: None,
            enabled: Some(true),
            created_at: None,
            created_by: Some(agent_did.clone()),
        });
    row.display_name = Some(display_name);
    row.default_behavior_id = Some(default_behavior_id);
    row.enabled = Some(request.enabled.unwrap_or(true));
    fixture.desktop_core().save_agent_principal(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn save_behavior_config(
    fixture: &LiveBridgeFixture,
    request: BehaviorSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let behavior_id = require_trimmed("behavior_id", request.behavior_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .behavior_row(&agent_did, &behavior_id)
        .cloned()
        .unwrap_or_else(|| AgentBehaviorRow {
            behavior_id: behavior_id.clone(),
            agent_did: Some(agent_did.clone()),
            display_name: None,
            system_prompt: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: Some(true),
            created_at: None,
        });
    let inference_profile_id = trim_optional(request.inference_profile_id)
        .ok_or_else(|| anyhow::anyhow!("inference_profile_id is required"))?;
    if !store
        .inference_profiles
        .iter()
        .any(|profile| profile.profile_id == inference_profile_id)
    {
        anyhow::bail!("inference_profile_id {inference_profile_id} does not exist");
    }
    row.display_name = Some(display_name);
    row.agent_did = Some(agent_did);
    row.system_prompt = Some(request.system_prompt);
    row.backend_id = trim_optional(request.backend_id);
    row.tool_selection_id = trim_optional(request.tool_selection_id);
    row.inference_profile_id = Some(inference_profile_id);
    row.compaction_strategy =
        trim_optional(request.compaction_strategy).or_else(|| row.compaction_strategy.clone());
    row.compaction_threshold = request.compaction_threshold.or(row.compaction_threshold);
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    if let Some(backend_id) = row.backend_id.as_deref() {
        if let Some(model_name) = store
            .inference_backends
            .iter()
            .find(|backend| backend.backend_id == backend_id)
            .and_then(|backend| backend.models.first())
            .cloned()
        {
            row.model_name = Some(model_name);
        }
    }
    fixture.desktop_core().save_behavior(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn save_backend_config(
    fixture: &LiveBridgeFixture,
    request: BackendSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let backend_id = require_trimmed("backend_id", request.backend_id)?;
    let name = require_trimmed("name", request.name)?;
    let provider_kind = require_trimmed("provider_kind", request.provider_kind)?;
    let endpoint = require_trimmed("endpoint", request.endpoint)?;
    let models = request
        .models
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        anyhow::bail!("at least one model is required");
    }

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .inference_backends
        .iter()
        .find(|row| row.backend_id == backend_id)
        .cloned()
        .unwrap_or_else(|| InferenceBackendRow {
            backend_id: backend_id.clone(),
            name: None,
            provider_kind: None,
            endpoint: None,
            api_key: None,
            api_key_env_var: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: Some(true),
            models: Vec::new(),
            last_probe: None,
            probe_status: None,
        });
    row.name = Some(name);
    row.provider_kind = Some(provider_kind);
    row.endpoint = Some(endpoint);
    if request.clear_api_key.unwrap_or(false) {
        row.api_key = None;
    } else if request.api_key.is_some() {
        row.api_key = trim_optional(request.api_key);
    }
    if request.api_key_env_var.is_some() {
        row.api_key_env_var = trim_optional(request.api_key_env_var);
    }
    row.models = models;
    row.max_concurrent = request.max_concurrent;
    row.max_queue_depth = request.max_queue_depth;
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    row.probe_status = Some("healthy".to_string());
    fixture.desktop_core().save_backend(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn save_inference_profile_config(
    fixture: &LiveBridgeFixture,
    request: InferenceProfileSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let profile_id = require_trimmed("profile_id", request.profile_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .inference_profiles
        .iter()
        .find(|row| row.profile_id == profile_id)
        .cloned()
        .unwrap_or_else(|| InferenceProfileRow {
            profile_id: profile_id.clone(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            max_turns: None,
            temperature: None,
            stream_batch_ms: None,
            deadline_duration_secs: None,
        });
    row.display_name = Some(display_name);
    row.context_window = request.context_window;
    row.max_output_tokens = request.max_output_tokens;
    row.max_turns = request.max_turns;
    row.temperature = request.temperature;
    row.stream_batch_ms = request.stream_batch_ms;
    row.deadline_duration_secs = request.deadline_duration_secs;
    fixture.desktop_core().save_inference_profile(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn save_tool_selection_config(
    fixture: &LiveBridgeFixture,
    request: ToolSelectionSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let selection_id = require_trimmed("selection_id", request.selection_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .tool_selections
        .iter()
        .find(|row| row.selection_id == selection_id)
        .cloned()
        .unwrap_or_else(|| ToolSelectionRow {
            selection_id: selection_id.clone(),
            agent_did: Some(agent_did.clone()),
            display_name: None,
            enable_file_tools: Some(false),
            file_tools_mode: None,
            file_tool_root: None,
            enable_bash: Some(false),
            bash_mode: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: Some(false),
            delegate_to: Vec::new(),
        });
    row.agent_did = Some(agent_did);
    row.display_name = Some(display_name);
    row.enable_file_tools = request.enable_file_tools.or(row.enable_file_tools);
    row.file_tools_mode = trim_optional(request.file_tools_mode);
    row.file_tool_root = trim_optional(request.file_tool_root);
    row.enable_bash = request.enable_bash.or(row.enable_bash);
    row.bash_mode = trim_optional(request.bash_mode);
    row.cli_tool_names = request
        .cli_tool_names
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    row.enable_meta_tools = request.enable_meta_tools.or(row.enable_meta_tools);
    row.delegate_to = request
        .delegate_to
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    fixture.desktop_core().save_tool_selection(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn save_tool_service_config(
    fixture: &LiveBridgeFixture,
    request: ToolServiceSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let service_id = require_trimmed("service_id", request.service_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .tool_service_registries
        .iter()
        .find(|row| row.service_id == service_id)
        .cloned()
        .unwrap_or_else(|| ToolServiceRegistryRow {
            service_id: service_id.clone(),
            display_name: None,
            description: None,
            hostname: None,
            tailscale_ip: None,
            lan_ip: None,
            mcp_port: None,
            mcp_path: Some("/mcp".to_string()),
            tools: Vec::new(),
            status: Some("online".to_string()),
            version: None,
            updated_at: None,
        });
    row.display_name = Some(display_name);
    row.description = trim_optional(request.description);
    row.hostname = trim_optional(request.hostname);
    row.tailscale_ip = trim_optional(request.tailscale_ip);
    row.lan_ip = trim_optional(request.lan_ip);
    row.mcp_port = request.mcp_port;
    row.mcp_path = trim_optional(request.mcp_path).or_else(|| Some("/mcp".to_string()));
    row.status = trim_optional(request.status)
        .or_else(|| row.status.clone())
        .or_else(|| Some("online".to_string()));
    fixture
        .desktop_core()
        .save_tool_service_registry(&row)
        .await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn test_tool_service_config(
    request: ToolServiceTestRequest,
) -> Result<ToolServiceTestResult> {
    let service_id = require_trimmed("service_id", request.service_id.clone())?;
    let endpoint = resolve_tool_service_endpoint(&request)?;
    let pool = McpPool::new();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        pool.list_tools(&service_id, &endpoint),
    )
    .await
    .context("MCP list_tools timed out")??;
    let tools = result
        .tools
        .iter()
        .map(|tool| ToolServiceToolView {
            name: tool.name.to_string(),
            description: tool.description.as_deref().map(str::to_owned),
        })
        .collect::<Vec<_>>();
    Ok(ToolServiceTestResult {
        service_id,
        endpoint,
        status: "ok".to_string(),
        tool_count: tools.len(),
        tools,
        error: None,
    })
}

async fn save_task_config(
    fixture: &LiveBridgeFixture,
    request: TaskSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let task_id = require_trimmed("task_id", request.task_id)?;
    let name = require_trimmed("name", request.name)?;
    let behavior_id = require_trimmed("behavior_id", request.behavior_id)?;
    let prompt_template = require_trimmed("prompt_template", request.prompt_template)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .tasks
        .iter()
        .find(|row| row.task_id == task_id)
        .cloned()
        .unwrap_or_else(|| TaskRow {
            task_id: task_id.clone(),
            name: None,
            description: None,
            behavior_id: None,
            prompt_template: None,
            enabled: Some(true),
            output_schema_ref: None,
            created_at: None,
            updated_at: None,
        });
    row.name = Some(name);
    row.description = trim_optional(request.description);
    row.behavior_id = Some(behavior_id);
    row.prompt_template = Some(prompt_template);
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    row.output_schema_ref = trim_optional(request.output_schema_ref);
    fixture.desktop_core().save_task(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn save_schedule_config(
    fixture: &LiveBridgeFixture,
    request: ScheduleSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let schedule_id = require_trimmed("schedule_id", request.schedule_id)?;
    let task_id = require_trimmed("task_id", request.task_id)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .schedules
        .iter()
        .find(|row| row.schedule_id == schedule_id)
        .cloned()
        .unwrap_or_else(|| ScheduleRow {
            schedule_id: schedule_id.clone(),
            task_id: Some(task_id.clone()),
            interval_secs: None,
            enabled: Some(true),
            concurrency: Some("serial".to_string()),
            next_run_at: None,
            last_attempt_at: None,
            last_status: None,
            last_error: None,
            fire_count: None,
            created_at: None,
            updated_at: None,
        });
    row.task_id = Some(task_id);
    row.interval_secs = request.interval_secs;
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    row.concurrency = trim_optional(request.concurrency).or_else(|| Some("serial".to_string()));
    fixture.desktop_core().save_schedule(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn run_schedule_config(
    fixture: &LiveBridgeFixture,
    request: ScheduleRunRequest,
) -> Result<TaskRunResult> {
    let schedule_id = require_trimmed("schedule_id", request.schedule_id)?;
    let store = fixture.desktop_core().store().snapshot();
    let schedule = store
        .schedules
        .iter()
        .find(|row| row.schedule_id == schedule_id)
        .cloned()
        .ok_or_else(|| anyhow!("schedule {schedule_id} was not found"))?;
    let request_doc_id = fixture.desktop_core().fire_schedule_now(&schedule).await?;
    let row =
        load_agent_request_by_doc_id(fixture.desktop_core().as_ref(), &request_doc_id).await?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        behavior_id: row.behavior_id.unwrap_or_default(),
        status: row.status,
        lifecycle_state: row.lifecycle_state,
    })
}

async fn save_event_trigger_config(
    fixture: &LiveBridgeFixture,
    request: EventTriggerSaveRequest,
) -> Result<DesktopClientSnapshot> {
    let trigger_id = require_trimmed("trigger_id", request.trigger_id)?;
    let task_id = require_trimmed("task_id", request.task_id)?;
    let source_collection = require_trimmed("source_collection", request.source_collection)?;
    let event_kind = require_trimmed("event_kind", request.event_kind)?;
    validate_event_kind(&event_kind)?;

    let store = fixture.desktop_core().store().snapshot();
    let mut row = store
        .event_triggers
        .iter()
        .find(|row| row.trigger_id == trigger_id)
        .cloned()
        .unwrap_or_else(|| EventTriggerRow {
            trigger_id: trigger_id.clone(),
            task_id: Some(task_id.clone()),
            source_collection: None,
            event_kind: None,
            filter: None,
            enabled: Some(true),
            concurrency: Some("serial".to_string()),
            created_at: None,
            updated_at: None,
            last_attempt_at: None,
            last_fired_source_doc_id: None,
            last_status: None,
            last_error: None,
            fire_count: None,
        });
    row.task_id = Some(task_id);
    row.source_collection = Some(source_collection);
    row.event_kind = Some(event_kind);
    row.filter = trim_optional(request.filter);
    row.enabled = request.enabled.or(row.enabled).or(Some(true));
    row.concurrency = trim_optional(request.concurrency).or_else(|| Some("serial".to_string()));
    fixture.desktop_core().save_event_trigger(&row).await?;

    Ok(build_desktop_client_snapshot(fixture).await)
}

async fn run_task_config(
    fixture: &LiveBridgeFixture,
    request: TaskRunRequest,
) -> Result<TaskRunResult> {
    let task_id = require_trimmed("task_id", request.task_id)?;
    let args = request.args.unwrap_or_else(|| serde_json::json!({}));
    let store = fixture.desktop_core().store().snapshot();
    let task = store
        .tasks
        .iter()
        .find(|row| row.task_id == task_id)
        .cloned()
        .ok_or_else(|| anyhow!("task {task_id} was not found"))?;
    let request_doc_id = fixture.desktop_core().fire_task_now(&task, args).await?;
    let row =
        load_agent_request_by_doc_id(fixture.desktop_core().as_ref(), &request_doc_id).await?;

    Ok(TaskRunResult {
        request_doc_id,
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        agent_did: row.agent_did.unwrap_or_default(),
        behavior_id: row.behavior_id.unwrap_or_default(),
        status: row.status,
        lifecycle_state: row.lifecycle_state,
    })
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buffer = Vec::new();
    let mut temp = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            anyhow::bail!("connection closed before headers");
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut content_length = 0_usize;
    for line in lines.clone() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or_default();
            }
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing request method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!("missing request path"))?
        .to_string();
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..read]);
    }
    let body =
        String::from_utf8_lossy(&buffer[header_end..buffer.len().min(header_end + content_length)])
            .to_string();

    Ok(HttpRequestData { method, path, body })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}
