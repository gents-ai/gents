use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde::Serialize;

use crate::bridge::commands::{
    add_peer, rename_conversation, repair_p2p, run_schedule_config, run_task_config,
    save_agent_config, save_backend_config, save_behavior_config, save_event_trigger_config,
    save_inference_profile_config, save_schedule_config, save_task_config,
    save_tool_selection_config, save_tool_service_config, send_chat_message,
    test_tool_service_config,
};
use crate::bridge::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, ChatSendRequest,
    ConversationRenameRequest, EventTriggerSaveRequest, InferenceProfileSaveRequest,
    PeerAddRequest, ScheduleRunRequest, ScheduleSaveRequest, TaskRunRequest, TaskSaveRequest,
    ToolSelectionSaveRequest, ToolServiceSaveRequest, ToolServiceTestRequest,
};
use crate::diagnostics::{
    build_desktop_client_snapshot, build_desktop_session_snapshot, build_request_diagnostics_bundle,
};
use crate::live_fixture::LiveBridgeFixture;

#[derive(Debug)]
struct HttpRequestData {
    method: String,
    path: String,
    body: String,
}

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

pub(crate) struct BridgeRunnerServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BridgeRunnerServer {
    pub(crate) fn start(fixture: Arc<LiveBridgeFixture>) -> Result<Self> {
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

    pub(crate) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) fn stop(mut self) {
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
            let request = serde_json::from_str::<PeerAddRequest>(&request.body)
                .context("decoding peer add request")?;
            runtime.block_on(add_peer(fixture.desktop_core().as_ref(), request))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/p2p/repair") => {
            runtime.block_on(repair_p2p(
                fixture.desktop_core().as_ref(),
                Duration::from_millis(250),
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
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
        ("POST", "/desktop/chat/send") => {
            let request = serde_json::from_str::<ChatSendRequest>(&request.body)
                .context("decoding chat send request")?;
            let result =
                runtime.block_on(send_chat_message(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/conversation/rename") => {
            let request = serde_json::from_str::<ConversationRenameRequest>(&request.body)
                .context("decoding rename request")?;
            runtime.block_on(rename_conversation(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "status": "ok" }).to_string(),
            ))
        }
        ("POST", "/desktop/agent/save") => {
            let request = serde_json::from_str::<AgentConfigSaveRequest>(&request.body)
                .context("decoding agent config save request")?;
            runtime.block_on(save_agent_config(fixture.desktop_core().as_ref(), request))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/behavior/save") => {
            let request = serde_json::from_str::<BehaviorSaveRequest>(&request.body)
                .context("decoding behavior save request")?;
            runtime.block_on(save_behavior_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/backend/save") => {
            let request = serde_json::from_str::<BackendSaveRequest>(&request.body)
                .context("decoding backend save request")?;
            runtime.block_on(save_backend_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/inference-profile/save") => {
            let request = serde_json::from_str::<InferenceProfileSaveRequest>(&request.body)
                .context("decoding inference profile save request")?;
            runtime.block_on(save_inference_profile_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/tool-selection/save") => {
            let request = serde_json::from_str::<ToolSelectionSaveRequest>(&request.body)
                .context("decoding tool selection save request")?;
            runtime.block_on(save_tool_selection_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/tool-service/save") => {
            let request = serde_json::from_str::<ToolServiceSaveRequest>(&request.body)
                .context("decoding tool service save request")?;
            runtime.block_on(save_tool_service_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
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
            runtime.block_on(save_task_config(fixture.desktop_core().as_ref(), request))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/schedule/save") => {
            let request = serde_json::from_str::<ScheduleSaveRequest>(&request.body)
                .context("decoding schedule save request")?;
            runtime.block_on(save_schedule_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/schedule/run") => {
            let request = serde_json::from_str::<ScheduleRunRequest>(&request.body)
                .context("decoding schedule run request")?;
            let result = runtime.block_on(run_schedule_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/event-trigger/save") => {
            let request = serde_json::from_str::<EventTriggerSaveRequest>(&request.body)
                .context("decoding event trigger save request")?;
            runtime.block_on(save_event_trigger_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/task/run") => {
            let request = serde_json::from_str::<TaskRunRequest>(&request.body)
                .context("decoding task run request")?;
            let result =
                runtime.block_on(run_task_config(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        _ => Ok(HttpResponse::json_error("404 Not Found", "not found")),
    }
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
