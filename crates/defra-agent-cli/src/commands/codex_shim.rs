use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use codex_app_server_protocol as codex;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::UpdateSubscriptionSource;
use defra_agent_protocol::transcript::present_persisted_message;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

mod progress;
mod trace;

use progress::{
    content_delta, decode_defra_tool_call_progress, decode_defra_turn_progress,
    defra_tool_call_status, defra_tool_item, defra_turn_progress_query, response_field_is_blank,
    terminal_error_message, terminal_turn_status, DefraToolCallProgress,
};

use crate::{
    create_agent_request, is_terminal_lifecycle_state, materialized_message_query,
    request_diagnostic_hint, RequestSubmitOptions, SubmittedRequest,
};

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

#[derive(Clone)]
struct ShimState {
    codex_home: PathBuf,
    trace_path: PathBuf,
    cwd: PathBuf,
    node: Arc<EmbeddedNode>,
    graphql: Arc<str>,
    agent_did: Arc<str>,
    behavior_id: Option<Arc<str>>,
    model: Arc<str>,
    id_counter: Arc<AtomicU64>,
    timeout: Duration,
    poll_interval: Duration,
}

pub(crate) struct CodexShimBindArgs {
    pub(crate) home: PathBuf,
    pub(crate) node: Arc<EmbeddedNode>,
    pub(crate) graphql: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
    pub(crate) bind_addr: std::net::IpAddr,
    pub(crate) port: u16,
    pub(crate) model: String,
    pub(crate) timeout_secs: u64,
    pub(crate) poll_ms: u64,
}

pub(crate) struct BoundCodexShim {
    addr: SocketAddr,
    codex_home: PathBuf,
    trace_path: PathBuf,
    listener: TcpListener,
    app: Router,
}

impl BoundCodexShim {
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub(crate) fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub(crate) fn trace_path(&self) -> &Path {
        &self.trace_path
    }

    pub(crate) fn spawn(self) -> JoinHandle<Result<()>> {
        tokio::spawn(self.serve())
    }

    async fn serve(self) -> Result<()> {
        axum::serve(self.listener, self.app)
            .await
            .context("serving Codex TUI shim")
    }
}

pub(crate) async fn bind_codex_shim(args: CodexShimBindArgs) -> Result<BoundCodexShim> {
    let codex_home = args.home.join("codex-ui");
    let codex_log_dir = codex_home.join("log");
    fs::create_dir_all(&codex_log_dir)
        .with_context(|| format!("creating Codex UI log dir {}", codex_log_dir.display()))?;
    let trace_path = codex_log_dir.join("codex-shim-events.jsonl");

    let state = ShimState {
        codex_home: codex_home.clone(),
        trace_path: trace_path.clone(),
        cwd: std::env::current_dir().context("resolving current working directory")?,
        node: args.node,
        graphql: Arc::from(args.graphql.clone()),
        agent_did: Arc::from(args.agent_did.clone()),
        behavior_id: args
            .behavior_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(Arc::from),
        model: Arc::from(args.model),
        id_counter: Arc::new(AtomicU64::new(1)),
        timeout: Duration::from_secs(args.timeout_secs),
        poll_interval: Duration::from_millis(args.poll_ms.max(1)),
    };

    let app = Router::new()
        .route("/", get(ws_upgrade))
        .with_state(state.clone());
    let addr = SocketAddr::new(args.bind_addr, args.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding Codex shim on {addr}"))?;

    Ok(BoundCodexShim {
        addr,
        codex_home,
        trace_path,
        listener,
        app,
    })
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<ShimState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: ShimState) {
    tracing::info!("Codex shim WebSocket connected");
    trace::shim_event(&state.trace_path, "websocket connected");
    while let Some(message) = socket.recv().await {
        let Ok(message) = message else {
            return;
        };
        let Message::Text(text) = message else {
            continue;
        };

        let Ok(payload) = serde_json::from_str::<codex::JSONRPCMessage>(&text) else {
            tracing::warn!("dropping invalid Codex shim JSON-RPC message");
            continue;
        };

        let result = match payload {
            codex::JSONRPCMessage::Request(request) => {
                handle_request(&mut socket, &state, request).await
            }
            codex::JSONRPCMessage::Notification(notification) => {
                tracing::trace!(?notification, "Codex shim received client notification");
                Ok(())
            }
            codex::JSONRPCMessage::Response(response) => {
                tracing::trace!(?response, "Codex shim received client response");
                Ok(())
            }
            codex::JSONRPCMessage::Error(error) => {
                tracing::trace!(?error, "Codex shim received client error");
                Ok(())
            }
        };

        if let Err(err) = result {
            tracing::warn!(%err, "Codex shim request handling failed");
            return;
        }
    }
}

async fn handle_request(
    socket: &mut WebSocket,
    state: &ShimState,
    request: codex::JSONRPCRequest,
) -> Result<()> {
    let request_id = request.id.clone();
    let method = request.method.clone();
    tracing::info!(%method, %request_id, "Codex shim request");
    trace::shim_event(&state.trace_path, format!("request {request_id} {method}"));
    let codex_request = match client_request_from_jsonrpc(request) {
        Ok(request) => request,
        Err(err) => {
            return send_error(
                socket,
                request_id,
                JSONRPC_INVALID_REQUEST,
                format!("invalid Codex shim request `{method}`: {err}"),
            )
            .await;
        }
    };

    match codex_request {
        codex::ClientRequest::Initialize { request_id, .. } => {
            send_typed_json_result::<codex::InitializeResponse>(
                socket,
                request_id,
                initialize_result(state),
            )
            .await
        }
        codex::ClientRequest::GetAccount { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::GetAccountResponse {
                    account: Some(codex::Account::ApiKey {}),
                    requires_openai_auth: false,
                },
            )
            .await
        }
        codex::ClientRequest::GetAccountRateLimits { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::GetAccountRateLimitsResponse {
                    rate_limits: empty_rate_limits(),
                    rate_limits_by_limit_id: None,
                },
            )
            .await
        }
        codex::ClientRequest::ModelList { request_id, .. } => {
            send_typed_json_result::<codex::ModelListResponse>(
                socket,
                request_id,
                json!({
                    "data": [model_summary(state)],
                    "nextCursor": null
                }),
            )
            .await
        }
        codex::ClientRequest::ModelProviderCapabilitiesRead { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ModelProviderCapabilitiesReadResponse {
                    namespace_tools: false,
                    image_generation: false,
                    web_search: false,
                },
            )
            .await
        }
        codex::ClientRequest::ConfigRead { request_id, .. } => {
            send_typed_json_result::<codex::ConfigReadResponse>(
                socket,
                request_id,
                json!({
                    "config": {
                        "model": state.model.as_ref(),
                        "model_provider": "defra",
                        "approval_policy": "never",
                        "sandbox_mode": "danger-full-access"
                    },
                    "origins": {}
                }),
            )
            .await
        }
        codex::ClientRequest::ConfigBatchWrite { request_id, .. }
        | codex::ClientRequest::ConfigValueWrite { request_id, .. } => {
            send_typed_json_result::<codex::ConfigWriteResponse>(
                socket,
                request_id,
                json!({
                    "status": "ok",
                    "version": "defra-shim",
                    "filePath": absolute_path(&state.codex_home.join("config.toml")),
                    "overriddenMetadata": null
                }),
            )
            .await
        }
        codex::ClientRequest::ConfigRequirementsRead { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ConfigRequirementsReadResponse { requirements: None },
            )
            .await
        }
        codex::ClientRequest::ExternalAgentConfigDetect { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ExternalAgentConfigDetectResponse { items: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::ExternalAgentConfigImport { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ExternalAgentConfigImportResponse {},
            )
            .await
        }
        codex::ClientRequest::ExperimentalFeatureList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ExperimentalFeatureListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::PermissionProfileList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::PermissionProfileListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::CollaborationModeList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::CollaborationModeListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::SkillsList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::SkillsListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::HooksList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::HooksListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::PluginList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::PluginListResponse {
                    marketplaces: Vec::new(),
                    marketplace_load_errors: Vec::new(),
                    featured_plugin_ids: Vec::new(),
                },
            )
            .await
        }
        codex::ClientRequest::McpServerStatusList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ListMcpServerStatusResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::ThreadStart {
            request_id, params, ..
        } => {
            let cwd = effective_cwd(state, params.cwd.as_deref());
            let thread_id = state.next_thread_id();
            let thread = thread_json(
                &cwd,
                &thread_id,
                None,
                codex::ThreadStatus::Idle,
                Vec::new(),
            );
            send_typed_json_result::<codex::ThreadStartResponse>(
                socket,
                request_id,
                json!({
                    "thread": thread,
                    "model": state.model.as_ref(),
                    "modelProvider": "defra",
                    "serviceTier": null,
                    "cwd": absolute_path(&cwd),
                    "runtimeWorkspaceRoots": [],
                    "instructionSources": [],
                    "approvalPolicy": "never",
                    "approvalsReviewer": "user",
                    "sandbox": { "type": "dangerFullAccess" },
                    "activePermissionProfile": null,
                    "reasoningEffort": null
                }),
            )
            .await
        }
        codex::ClientRequest::ThreadList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ThreadListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                    backwards_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::ThreadLoadedList { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ThreadLoadedListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::ThreadRead {
            request_id, params, ..
        } => {
            send_typed_json_result::<codex::ThreadReadResponse>(
                socket,
                request_id,
                json!({
                    "thread": thread_json(
                        &state.cwd,
                        &params.thread_id,
                        None,
                        codex::ThreadStatus::Idle,
                        Vec::new()
                    )
                }),
            )
            .await
        }
        codex::ClientRequest::ThreadUnsubscribe { request_id, .. } => {
            send_result(
                socket,
                request_id,
                codex::ThreadUnsubscribeResponse {
                    status: codex::ThreadUnsubscribeStatus::Unsubscribed,
                },
            )
            .await
        }
        codex::ClientRequest::TurnStart {
            request_id, params, ..
        } => {
            start_defra_turn(
                socket,
                state,
                request_id,
                params.thread_id,
                params.input,
                true,
            )
            .await
        }
        codex::ClientRequest::TurnSteer {
            request_id, params, ..
        } => {
            start_defra_turn(
                socket,
                state,
                request_id,
                params.thread_id,
                params.input,
                false,
            )
            .await
        }
        codex::ClientRequest::TurnInterrupt { request_id, .. } => {
            send_result(socket, request_id, codex::TurnInterruptResponse {}).await
        }
        unsupported => {
            let request_id = unsupported.id().clone();
            send_error(
                socket,
                request_id,
                JSONRPC_METHOD_NOT_FOUND,
                format!("unsupported Codex shim method `{}`", unsupported.method()),
            )
            .await
        }
    }
}

async fn start_defra_turn(
    socket: &mut WebSocket,
    state: &ShimState,
    request_id: codex::RequestId,
    thread_id: String,
    input: Vec<codex::UserInput>,
    start_response: bool,
) -> Result<()> {
    let user_text = user_text_from_input(&input);
    if user_text.trim().is_empty() {
        return send_error(
            socket,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "Codex turn input did not contain text for DEFRA".to_string(),
        )
        .await;
    }

    let submitted = match create_agent_request(
        state.graphql.as_ref(),
        state.agent_did.as_ref(),
        &user_text,
        Some(&thread_id),
        state.behavior_id.as_deref(),
        RequestSubmitOptions::default(),
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(err) => {
            return send_error(
                socket,
                request_id,
                JSONRPC_INTERNAL_ERROR,
                format!("failed to submit DEFRA AgentRequest: {err}"),
            )
            .await;
        }
    };

    let turn_id = submitted.request_id.clone();
    let started_turn = turn_value(&turn_id, codex::TurnStatus::InProgress, Vec::new(), None);

    if start_response {
        send_result(
            socket,
            request_id,
            codex::TurnStartResponse {
                turn: started_turn.clone(),
            },
        )
        .await?;
    } else {
        send_result(
            socket,
            request_id,
            codex::TurnSteerResponse {
                turn_id: turn_id.clone(),
            },
        )
        .await?;
    }

    send_notification(
        socket,
        state,
        codex::ServerNotification::TurnStarted(codex::TurnStartedNotification {
            thread_id: thread_id.clone(),
            turn: started_turn,
        }),
    )
    .await?;

    let mut projection = TurnProjection::new(state, &thread_id, &turn_id);

    match stream_defra_turn(socket, state, &submitted, &mut projection).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let message = format!("DEFRA turn failed: {err}");
            projection
                .append_agent_delta(socket, &format!("[agent error] {message}\n"))
                .await?;
            projection
                .finish_turn(socket, codex::TurnStatus::Failed, Some(message))
                .await
        }
    }
}

struct TurnProjection<'a> {
    state: &'a ShimState,
    thread_id: &'a str,
    turn_id: &'a str,
    active_agent_item_id: Option<String>,
    active_agent_text: String,
    rendered_agent_text: String,
    completed_items: Vec<codex::ThreadItem>,
}

impl<'a> TurnProjection<'a> {
    fn new(state: &'a ShimState, thread_id: &'a str, turn_id: &'a str) -> Self {
        Self {
            state,
            thread_id,
            turn_id,
            active_agent_item_id: None,
            active_agent_text: String::new(),
            rendered_agent_text: String::new(),
            completed_items: Vec::new(),
        }
    }

    async fn append_agent_delta(&mut self, socket: &mut WebSocket, delta: &str) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        let delta = if self.rendered_agent_text.is_empty() {
            delta.trim_start()
        } else {
            delta
        };
        if delta.is_empty() {
            return Ok(());
        }
        let item_id = if let Some(item_id) = self.active_agent_item_id.as_ref() {
            item_id.clone()
        } else {
            let item_id = self.state.next_id("defra-message");
            send_notification(
                socket,
                self.state,
                codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                    item: agent_message_item(&item_id, ""),
                    thread_id: self.thread_id.to_string(),
                    turn_id: self.turn_id.to_string(),
                    started_at_ms: now_millis(),
                }),
            )
            .await?;
            self.active_agent_item_id = Some(item_id.clone());
            item_id
        };

        self.active_agent_text.push_str(delta);
        self.rendered_agent_text.push_str(delta);
        send_notification(
            socket,
            self.state,
            codex::ServerNotification::AgentMessageDelta(codex::AgentMessageDeltaNotification {
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                item_id,
                delta: delta.to_string(),
            }),
        )
        .await
    }

    async fn finish_agent_message(&mut self, socket: &mut WebSocket) -> Result<()> {
        let Some(item_id) = self.active_agent_item_id.take() else {
            return Ok(());
        };
        let text = std::mem::take(&mut self.active_agent_text);
        if text.trim().is_empty() {
            return Ok(());
        }
        let completed_item = agent_message_item(&item_id, &text);
        send_notification(
            socket,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: completed_item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: now_millis(),
            }),
        )
        .await?;
        self.completed_items.push(completed_item);
        Ok(())
    }

    async fn send_tool_started(
        &mut self,
        socket: &mut WebSocket,
        tool: &DefraToolCallProgress,
    ) -> Result<()> {
        self.finish_agent_message(socket).await?;
        send_notification(
            socket,
            self.state,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item: defra_tool_item(tool, codex::McpToolCallStatus::InProgress),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                started_at_ms: now_millis(),
            }),
        )
        .await
    }

    async fn send_tool_completed(
        &mut self,
        socket: &mut WebSocket,
        tool: &DefraToolCallProgress,
        status: codex::McpToolCallStatus,
    ) -> Result<()> {
        self.finish_agent_message(socket).await?;
        let completed_item = defra_tool_item(tool, status);
        send_notification(
            socket,
            self.state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: completed_item.clone(),
                thread_id: self.thread_id.to_string(),
                turn_id: self.turn_id.to_string(),
                completed_at_ms: now_millis(),
            }),
        )
        .await?;
        self.completed_items.push(completed_item);
        Ok(())
    }

    async fn finish_turn(
        &mut self,
        socket: &mut WebSocket,
        status: codex::TurnStatus,
        error_message: Option<String>,
    ) -> Result<()> {
        self.finish_agent_message(socket).await?;
        let turn_error = if status == codex::TurnStatus::Failed {
            Some(codex::TurnError {
                message: error_message.unwrap_or_else(|| "DEFRA turn failed".to_string()),
                codex_error_info: None,
                additional_details: None,
            })
        } else {
            None
        };
        send_notification(
            socket,
            self.state,
            codex::ServerNotification::TurnCompleted(codex::TurnCompletedNotification {
                thread_id: self.thread_id.to_string(),
                turn: turn_value(
                    self.turn_id,
                    status,
                    std::mem::take(&mut self.completed_items),
                    turn_error,
                ),
            }),
        )
        .await
    }

    fn active_agent_text(&self) -> &str {
        &self.active_agent_text
    }

    fn rendered_agent_text(&self) -> &str {
        &self.rendered_agent_text
    }
}

async fn stream_defra_turn(
    socket: &mut WebSocket,
    state: &ShimState,
    submitted: &SubmittedRequest,
    projection: &mut TurnProjection<'_>,
) -> Result<()> {
    let mut known_tool_calls: BTreeMap<String, codex::McpToolCallStatus> = BTreeMap::new();
    let mut updates = state.node.subscribe_updates();
    let mut latest_content = String::new();
    let mut latest_reasoning = String::new();
    let mut latest_error_message: Option<String> = None;
    let mut latest_progress_signature: Option<String> = None;
    let mut last_progress_at = tokio::time::Instant::now();

    loop {
        let response = query_node_json(
            state.node.as_ref(),
            &defra_turn_progress_query(&submitted.request_id, &submitted.session_id),
        )
        .await?;
        let request_row = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();
        let response_row = response
            .pointer("/data/AgentResponse")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();

        let signature = serde_json::to_string(&json!({
            "request": &request_row,
            "response": &response_row,
            "tools": response.pointer("/data/AgentToolCall"),
        }))
        .context("serializing DEFRA Codex shim progress signature")?;
        if latest_progress_signature.as_deref() != Some(signature.as_str()) {
            latest_progress_signature = Some(signature);
            last_progress_at = tokio::time::Instant::now();
        }

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tool_rows.iter().filter_map(decode_defra_tool_call_progress) {
            let codex_status = defra_tool_call_status(&tool);
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            if previous_status.as_ref() == Some(&codex_status) {
                continue;
            }

            if previous_status.is_none() && codex_status != codex::McpToolCallStatus::InProgress {
                projection.send_tool_started(socket, &tool).await?;
            }

            match codex_status {
                codex::McpToolCallStatus::InProgress => {
                    projection.send_tool_started(socket, &tool).await?;
                }
                codex::McpToolCallStatus::Completed | codex::McpToolCallStatus::Failed => {
                    projection
                        .send_tool_completed(socket, &tool, codex_status.clone())
                        .await?;
                }
            }

            known_tool_calls.insert(tool.tool_call_key.clone(), codex_status);
        }

        let response_progress = response_row.as_ref().and_then(decode_defra_turn_progress);
        if let Some(progress) = response_progress.as_ref() {
            if progress.content != latest_content {
                let delta = content_delta(&latest_content, &progress.content);
                latest_content = progress.content.clone();
                projection.append_agent_delta(socket, &delta).await?;
            }
            latest_reasoning = progress.reasoning.clone();
            latest_error_message = progress.error_message.clone();
        }

        let lifecycle_state = request_row
            .as_ref()
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let failure_reason = request_row
            .as_ref()
            .and_then(|row| row.get("failure_reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let response_status = response_progress
            .as_ref()
            .map(|progress| progress.status.as_str())
            .unwrap_or("");
        let terminal_by_request = is_terminal_lifecycle_state(lifecycle_state);
        let terminal_by_response = matches!(response_status, "complete" | "completed" | "error");

        if terminal_by_request || terminal_by_response {
            let mut terminal_response = response_row.unwrap_or_else(|| {
                json!({
                    "request_id": submitted.request_id,
                    "status": null,
                    "content": null,
                })
            });
            let should_wait_for_materialized_content =
                matches!(response_status, "complete" | "completed")
                    && response_field_is_blank(&terminal_response, "content")
                    && terminal_response
                        .get("materialized_message_sequence")
                        .is_some_and(|value| !value.is_null());
            let hydrated =
                hydrate_materialized_response_content(state.node.as_ref(), &mut terminal_response)
                    .await?;
            if should_wait_for_materialized_content && !hydrated {
                if last_progress_at.elapsed() >= state.timeout {
                    anyhow::bail!(
                        "timed out waiting for materialized AgentMessage {} after {}s of inactivity\n{}",
                        submitted.request_id,
                        state.timeout.as_secs(),
                        request_diagnostic_hint(&submitted.request_id)
                    );
                }
                tokio::time::sleep(state.poll_interval).await;
                continue;
            }

            if let Some(content) = terminal_response.get("content").and_then(Value::as_str) {
                let delta = content_delta(projection.active_agent_text(), content);
                projection.append_agent_delta(socket, &delta).await?;
            }

            let error_message = terminal_error_message(
                response_status,
                latest_error_message.as_deref(),
                lifecycle_state,
                failure_reason,
            );
            if let Some(error_message) = error_message.as_deref() {
                if !projection.rendered_agent_text().contains(error_message) {
                    projection
                        .append_agent_delta(socket, &format!("\n[agent error] {error_message}\n"))
                        .await?;
                }
            }

            let turn_status = terminal_turn_status(lifecycle_state, response_status);
            return projection
                .finish_turn(socket, turn_status, error_message)
                .await;
        }

        if last_progress_at.elapsed() >= state.timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                submitted.request_id,
                state.timeout.as_secs(),
                request_diagnostic_hint(&submitted.request_id)
            );
        }

        let _ = &latest_reasoning;
        tokio::select! {
            _ = tokio::time::sleep(state.poll_interval) => {}
            msg = updates.recv() => {
                if msg.is_none() {
                    tracing::warn!("Codex shim embedded-node update subscription closed");
                }
                let dropped = updates.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "Codex shim update subscription dropped messages");
                }
            }
        }
    }
}

async fn query_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("DEFRA Codex shim query failed: {:?}", response.errors);
    }
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
}

async fn hydrate_materialized_response_content(
    node: &EmbeddedNode,
    response: &mut Value,
) -> Result<bool> {
    let content_blank = response_field_is_blank(response, "content");
    let reasoning_blank = response_field_is_blank(response, "reasoning");
    if !content_blank && !reasoning_blank {
        return Ok(true);
    }

    let Some(sequence) = response_materialized_sequence(response) else {
        return Ok(!content_blank || !reasoning_blank);
    };
    let Some(session_id) = response.get("session_id").and_then(Value::as_str) else {
        return Ok(!content_blank || !reasoning_blank);
    };

    let message_response =
        query_node_json(node, &materialized_message_query(session_id, sequence)).await?;
    let Some(message) = message_response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return Ok(false);
    };
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(false);
    };
    let Some(content) = message.get("content").and_then(Value::as_str) else {
        return Ok(false);
    };

    let presentation = present_persisted_message(role, content);
    let Some(object) = response.as_object_mut() else {
        return Ok(false);
    };

    if content_blank && !presentation.body_markdown.trim().is_empty() {
        object.insert(
            "content".to_string(),
            Value::String(presentation.body_markdown),
        );
    }
    if reasoning_blank {
        if let Some(reasoning) = presentation
            .reasoning_markdown
            .filter(|value| !value.trim().is_empty())
        {
            object.insert("reasoning".to_string(), Value::String(reasoning));
        }
    }

    Ok(!response_field_is_blank(response, "content")
        || !response_field_is_blank(response, "reasoning"))
}

fn response_materialized_sequence(response: &Value) -> Option<i64> {
    response
        .get("materialized_message_sequence")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
}

fn client_request_from_jsonrpc(
    request: codex::JSONRPCRequest,
) -> std::result::Result<codex::ClientRequest, serde_json::Error> {
    serde_json::from_value(serde_json::to_value(request)?)
}

async fn send_typed_json_result<T>(
    socket: &mut WebSocket,
    id: codex::RequestId,
    value: Value,
) -> Result<()>
where
    T: DeserializeOwned + Serialize,
{
    let response = serde_json::from_value::<T>(value)
        .with_context(|| format!("validating Codex response {}", std::any::type_name::<T>()))?;
    send_result(socket, id, response).await
}

async fn send_result<T>(socket: &mut WebSocket, id: codex::RequestId, response: T) -> Result<()>
where
    T: Serialize,
{
    let result = serde_json::to_value(response).context("serializing Codex response payload")?;
    send_json(socket, &codex::JSONRPCResponse { id, result }).await
}

async fn send_error(
    socket: &mut WebSocket,
    id: codex::RequestId,
    code: i64,
    message: String,
) -> Result<()> {
    send_json(
        socket,
        &codex::JSONRPCError {
            id,
            error: codex::JSONRPCErrorError {
                code,
                data: None,
                message,
            },
        },
    )
    .await
}

async fn send_notification(
    socket: &mut WebSocket,
    state: &ShimState,
    notification: codex::ServerNotification,
) -> Result<()> {
    trace::codex_notification(&state.trace_path, &notification);
    send_json(socket, &notification).await
}

async fn send_json<T>(socket: &mut WebSocket, value: &T) -> Result<()>
where
    T: Serialize,
{
    let text = serde_json::to_string(value).context("serializing Codex shim WebSocket message")?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("sending Codex shim WebSocket message")
}

fn initialize_result(state: &ShimState) -> Value {
    json!({
        "userAgent": concat!("defra-agent-codex-shim/", env!("CARGO_PKG_VERSION")),
        "codexHome": absolute_path(&state.codex_home),
        "platformFamily": std::env::consts::FAMILY,
        "platformOs": std::env::consts::OS
    })
}

fn model_summary(state: &ShimState) -> Value {
    json!({
        "id": state.model.as_ref(),
        "model": state.model.as_ref(),
        "upgrade": null,
        "upgradeInfo": null,
        "availabilityNux": null,
        "displayName": "DEFRA default",
        "description": "Synthetic model exposed by defra-agent codex-shim",
        "hidden": false,
        "supportedReasoningEfforts": [],
        "defaultReasoningEffort": "medium",
        "inputModalities": ["text"],
        "supportsPersonality": false,
        "additionalSpeedTiers": [],
        "serviceTiers": [],
        "defaultServiceTier": null,
        "isDefault": true
    })
}

fn thread_json(
    cwd: &Path,
    thread_id: &str,
    preview: Option<&str>,
    status: codex::ThreadStatus,
    turns: Vec<codex::Turn>,
) -> Value {
    let now = now_seconds();
    json!({
        "id": thread_id,
        "sessionId": thread_id,
        "forkedFromId": null,
        "preview": preview.unwrap_or(""),
        "ephemeral": false,
        "modelProvider": "defra",
        "createdAt": now,
        "updatedAt": now,
        "status": status,
        "path": null,
        "cwd": absolute_path(cwd),
        "cliVersion": env!("CARGO_PKG_VERSION"),
        "source": "cli",
        "threadSource": null,
        "agentNickname": null,
        "agentRole": null,
        "gitInfo": null,
        "name": null,
        "turns": turns
    })
}

fn turn_value(
    turn_id: &str,
    status: codex::TurnStatus,
    items: Vec<codex::ThreadItem>,
    error: Option<codex::TurnError>,
) -> codex::Turn {
    let now = now_seconds();
    let completed_at = (!matches!(status, codex::TurnStatus::InProgress)).then_some(now);
    codex::Turn {
        id: turn_id.to_string(),
        items,
        items_view: codex::TurnItemsView::Full,
        status,
        error,
        started_at: Some(now),
        completed_at,
        duration_ms: None,
    }
}

fn agent_message_item(item_id: &str, text: &str) -> codex::ThreadItem {
    codex::ThreadItem::AgentMessage {
        id: item_id.to_string(),
        text: text.to_string(),
        phase: None,
        memory_citation: None,
    }
}

fn empty_rate_limits() -> codex::RateLimitSnapshot {
    codex::RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

fn user_text_from_input(input: &[codex::UserInput]) -> String {
    input
        .iter()
        .filter_map(|item| match item {
            codex::UserInput::Text { text, .. } => Some(text.as_str()),
            codex::UserInput::Image { .. }
            | codex::UserInput::LocalImage { .. }
            | codex::UserInput::Skill { .. }
            | codex::UserInput::Mention { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn effective_cwd(state: &ShimState, cwd: Option<&str>) -> PathBuf {
    let Some(cwd) = cwd else {
        return state.cwd.clone();
    };
    let path = PathBuf::from(cwd);
    if path.is_absolute() {
        path
    } else {
        state.cwd.join(path)
    }
}

fn absolute_path(path: &Path) -> String {
    if path.is_absolute() {
        path.display().to_string()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl ShimState {
    fn next_thread_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn next_id(&self, prefix: &str) -> String {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_items_from_codex_turn_payload() {
        let input = vec![
            codex::UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            },
            codex::UserInput::Image {
                detail: None,
                url: "https://example.invalid/image.png".to_string(),
            },
            codex::UserInput::Text {
                text: "world".to_string(),
                text_elements: Vec::new(),
            },
        ];

        assert_eq!(user_text_from_input(&input), "hello\nworld");
    }

    #[test]
    fn thread_status_uses_codex_tag_shape() {
        let thread = thread_json(
            Path::new("/tmp"),
            "thread-1",
            Some("preview"),
            codex::ThreadStatus::Idle,
            Vec::new(),
        );
        let typed: codex::Thread = serde_json::from_value(thread).unwrap();
        let serialized = serde_json::to_value(typed).unwrap();

        assert_eq!(serialized.pointer("/status/type"), Some(&json!("idle")));
        assert_eq!(serialized.pointer("/source"), Some(&json!("cli")));
    }

    #[test]
    fn defra_tool_errors_render_as_failed_codex_tool_calls() {
        let tool = DefraToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: "glob".to_string(),
            status: "completed".to_string(),
            args: r#"{"pattern":"**/*.lean"}"#.to_string(),
            result: "Toolset error: missing runner".to_string(),
        };

        assert_eq!(
            defra_tool_call_status(&tool),
            codex::McpToolCallStatus::Failed
        );
        let item = defra_tool_item(&tool, codex::McpToolCallStatus::Failed);
        let codex::ThreadItem::McpToolCall {
            server,
            tool: tool_name,
            arguments,
            status,
            error,
            ..
        } = item
        else {
            panic!("expected MCP tool call item");
        };
        assert_eq!(server, "defra");
        assert_eq!(tool_name, "glob");
        assert_eq!(arguments["pattern"], "**/*.lean");
        assert_eq!(status, codex::McpToolCallStatus::Failed);
        assert_eq!(
            error.expect("failed tool should carry error").message,
            "Toolset error: missing runner"
        );
    }

    #[test]
    fn content_delta_ignores_terminal_leading_whitespace_normalization() {
        assert_eq!(
            content_delta("\n\nAnswer with context", "Answer with context"),
            ""
        );
        assert_eq!(
            content_delta("\n\nAnswer", "Answer with context"),
            " with context"
        );
    }
}
