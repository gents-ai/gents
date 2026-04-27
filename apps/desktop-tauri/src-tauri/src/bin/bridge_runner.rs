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
use serde::Deserialize;
use serde::Serialize;

use bridge::snapshot::{build_runtime_snapshot, build_session_snapshot_from_store};
use bridge::types::{
    ChatSendRequest, ChatSendResult, ConversationRenameRequest, DesktopClientSnapshot,
    DesktopSessionSnapshot,
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
    core: &defra_agent_desktop::client::ClientCore,
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
    core: &defra_agent_desktop::client::ClientCore,
) -> Option<String> {
    match tokio::time::timeout(Duration::from_secs(5), core.refresh_store()).await {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("timed out refreshing store".to_string()),
    }
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
