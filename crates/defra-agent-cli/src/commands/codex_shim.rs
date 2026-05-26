use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
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
use defra_agent::graphql::escape_graphql_string;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::cli::CodexShimArgs;
use crate::home_state::{resolve_agent_did, resolve_graphql_endpoint, resolve_home_dir};
use crate::{
    create_agent_request, hydrate_materialized_response_content, is_terminal_lifecycle_state,
    post_graphql, request_diagnostic_hint, RequestSubmitOptions, SubmittedRequest,
};

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

#[derive(Clone)]
struct ShimState {
    codex_home: PathBuf,
    cwd: PathBuf,
    graphql: Arc<str>,
    agent_did: Arc<str>,
    behavior_id: Option<Arc<str>>,
    model: Arc<str>,
    id_counter: Arc<AtomicU64>,
    timeout: Duration,
    poll_interval: Duration,
}

pub(crate) async fn codex_shim(args: CodexShimArgs) -> Result<()> {
    let bound = bind_codex_shim(args).await?;
    bound.print_startup();
    bound.serve().await
}

pub(crate) struct BoundCodexShim {
    addr: SocketAddr,
    codex_home: PathBuf,
    graphql: String,
    agent_did: String,
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

    pub(crate) fn print_startup(&self) {
        println!("Codex TUI shim listening on ws://{}/", self.addr);
        println!("DEFRA GraphQL endpoint: {}", self.graphql);
        println!("DEFRA agent DID: {}", self.agent_did);
        println!("Suggested launch:");
        println!(
            "  CODEX_HOME={} codex --dangerously-bypass-approvals-and-sandbox --remote ws://{}/",
            self.codex_home.display(),
            self.addr
        );
        println!(
            "Note: stock Codex may run local onboarding before connecting when CODEX_HOME is empty."
        );
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

pub(crate) async fn bind_codex_shim(args: CodexShimArgs) -> Result<BoundCodexShim> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let codex_home = home_dir.join("codex-ui");
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("creating Codex UI home {}", codex_home.display()))?;
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;

    let state = ShimState {
        codex_home: codex_home.clone(),
        cwd: std::env::current_dir().context("resolving current working directory")?,
        graphql: Arc::from(graphql.clone()),
        agent_did: Arc::from(agent_did.clone()),
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
        graphql,
        agent_did,
        listener,
        app,
    })
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<ShimState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: ShimState) {
    tracing::info!("Codex shim WebSocket connected");
    trace_shim_event("websocket connected");
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
    trace_shim_event(format!("request {request_id} {method}"));
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
    let item_id = state.next_id("defra-message");
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
        codex::ServerNotification::TurnStarted(codex::TurnStartedNotification {
            thread_id: thread_id.clone(),
            turn: started_turn,
        }),
    )
    .await?;

    let mut rendered_text = String::new();
    let mut agent_item_started = false;

    match stream_defra_turn(
        socket,
        state,
        &thread_id,
        &turn_id,
        &item_id,
        &submitted,
        &mut rendered_text,
        &mut agent_item_started,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(err) => {
            let message = format!("DEFRA turn failed: {err}");
            append_agent_delta(
                socket,
                &thread_id,
                &turn_id,
                &item_id,
                &mut rendered_text,
                &mut agent_item_started,
                &format!("[agent error] {message}\n"),
            )
            .await?;
            finish_defra_turn(
                socket,
                &thread_id,
                &turn_id,
                &item_id,
                rendered_text,
                agent_item_started,
                codex::TurnStatus::Failed,
                Some(message),
            )
            .await
        }
    }
}

async fn stream_defra_turn(
    socket: &mut WebSocket,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
    item_id: &str,
    submitted: &SubmittedRequest,
    rendered_text: &mut String,
    agent_item_started: &mut bool,
) -> Result<()> {
    let mut known_tool_calls: BTreeMap<String, codex::McpToolCallStatus> = BTreeMap::new();
    let mut latest_content = String::new();
    let mut latest_reasoning = String::new();
    let mut latest_error_message: Option<String> = None;
    let mut latest_progress_signature: Option<String> = None;
    let mut last_progress_at = tokio::time::Instant::now();

    loop {
        let response = post_graphql(
            state.graphql.as_ref(),
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
                send_defra_tool_started(socket, thread_id, turn_id, &tool).await?;
            }

            match codex_status {
                codex::McpToolCallStatus::InProgress => {
                    send_defra_tool_started(socket, thread_id, turn_id, &tool).await?;
                }
                codex::McpToolCallStatus::Completed | codex::McpToolCallStatus::Failed => {
                    send_defra_tool_completed(
                        socket,
                        thread_id,
                        turn_id,
                        &tool,
                        codex_status.clone(),
                    )
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
                append_agent_delta(
                    socket,
                    thread_id,
                    turn_id,
                    item_id,
                    rendered_text,
                    agent_item_started,
                    &delta,
                )
                .await?;
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
            let hydrated = hydrate_materialized_response_content(
                state.graphql.as_ref(),
                &mut terminal_response,
            )
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
                let delta = content_delta(rendered_text, content);
                append_agent_delta(
                    socket,
                    thread_id,
                    turn_id,
                    item_id,
                    rendered_text,
                    agent_item_started,
                    &delta,
                )
                .await?;
            }

            let error_message = terminal_error_message(
                response_status,
                latest_error_message.as_deref(),
                lifecycle_state,
                failure_reason,
            );
            if let Some(error_message) = error_message.as_deref() {
                if !rendered_text.contains(error_message) {
                    append_agent_delta(
                        socket,
                        thread_id,
                        turn_id,
                        item_id,
                        rendered_text,
                        agent_item_started,
                        &format!("\n[agent error] {error_message}\n"),
                    )
                    .await?;
                }
            }

            let turn_status = terminal_turn_status(lifecycle_state, response_status);
            return finish_defra_turn(
                socket,
                thread_id,
                turn_id,
                item_id,
                std::mem::take(rendered_text),
                *agent_item_started,
                turn_status,
                error_message,
            )
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
        tokio::time::sleep(state.poll_interval).await;
    }
}

async fn append_agent_delta(
    socket: &mut WebSocket,
    thread_id: &str,
    turn_id: &str,
    item_id: &str,
    rendered_text: &mut String,
    agent_item_started: &mut bool,
    delta: &str,
) -> Result<()> {
    if delta.is_empty() {
        return Ok(());
    }
    let delta = if rendered_text.is_empty() {
        delta.trim_start()
    } else {
        delta
    };
    if delta.is_empty() {
        return Ok(());
    }
    if !*agent_item_started {
        send_notification(
            socket,
            codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
                item: agent_message_item(item_id, ""),
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                started_at_ms: now_millis(),
            }),
        )
        .await?;
        *agent_item_started = true;
    }
    rendered_text.push_str(delta);
    send_notification(
        socket,
        codex::ServerNotification::AgentMessageDelta(codex::AgentMessageDeltaNotification {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            item_id: item_id.to_string(),
            delta: delta.to_string(),
        }),
    )
    .await
}

async fn send_defra_tool_started(
    socket: &mut WebSocket,
    thread_id: &str,
    turn_id: &str,
    tool: &DefraToolCallProgress,
) -> Result<()> {
    send_notification(
        socket,
        codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
            item: defra_tool_item(tool, codex::McpToolCallStatus::InProgress),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            started_at_ms: now_millis(),
        }),
    )
    .await
}

async fn send_defra_tool_completed(
    socket: &mut WebSocket,
    thread_id: &str,
    turn_id: &str,
    tool: &DefraToolCallProgress,
    status: codex::McpToolCallStatus,
) -> Result<()> {
    send_notification(
        socket,
        codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
            item: defra_tool_item(tool, status),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            completed_at_ms: now_millis(),
        }),
    )
    .await
}

async fn finish_defra_turn(
    socket: &mut WebSocket,
    thread_id: &str,
    turn_id: &str,
    item_id: &str,
    rendered_text: String,
    agent_item_started: bool,
    status: codex::TurnStatus,
    error_message: Option<String>,
) -> Result<()> {
    let turn_error = if status == codex::TurnStatus::Failed {
        Some(codex::TurnError {
            message: error_message.unwrap_or_else(|| "DEFRA turn failed".to_string()),
            codex_error_info: None,
            additional_details: None,
        })
    } else {
        None
    };
    let completed_item = agent_message_item(item_id, &rendered_text);
    if agent_item_started || !rendered_text.trim().is_empty() {
        send_notification(
            socket,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: completed_item.clone(),
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                completed_at_ms: now_millis(),
            }),
        )
        .await?;
    }
    send_notification(
        socket,
        codex::ServerNotification::TurnCompleted(codex::TurnCompletedNotification {
            thread_id: thread_id.to_string(),
            turn: turn_value(
                turn_id,
                status,
                if rendered_text.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![completed_item]
                },
                turn_error,
            ),
        }),
    )
    .await
}

#[derive(Debug, Clone)]
struct DefraTurnProgress {
    content: String,
    reasoning: String,
    error_message: Option<String>,
    status: String,
}

#[derive(Debug, Clone)]
struct DefraToolCallProgress {
    tool_call_key: String,
    tool_name: String,
    status: String,
    args: String,
    result: String,
}

fn defra_turn_progress_query(request_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                lifecycle_state
                failure_reason
                interrupt_requested_at
                valid_until
            }}
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                session_id
                status
                content
                reasoning
                error_message
                progress_seq
                materialized_message_sequence
                materialized_at
                completed_at
                interrupted_at
            }}
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    request_id: {{ _eq: "{request_id}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_key
                tool_name
                status
                args
                result
                started_at
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
    )
}

fn decode_defra_turn_progress(row: &Value) -> Option<DefraTurnProgress> {
    Some(DefraTurnProgress {
        content: row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        reasoning: row
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        error_message: row
            .get("error_message")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned),
        status: row.get("status")?.as_str()?.to_string(),
    })
}

fn decode_defra_tool_call_progress(row: &Value) -> Option<DefraToolCallProgress> {
    Some(DefraToolCallProgress {
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        args: row
            .get("args")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: row
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn defra_tool_item(
    tool: &DefraToolCallProgress,
    status: codex::McpToolCallStatus,
) -> codex::ThreadItem {
    let (result, error) = match status {
        codex::McpToolCallStatus::Completed => (
            Some(Box::new(codex::McpToolCallResult {
                content: defra_tool_result_content(&tool.result),
                structured_content: parse_json_value(&tool.result),
                meta: None,
            })),
            None,
        ),
        codex::McpToolCallStatus::Failed => (
            None,
            Some(codex::McpToolCallError {
                message: preview_compact_text(&tool.result)
                    .unwrap_or_else(|| "DEFRA tool call failed".to_string()),
            }),
        ),
        codex::McpToolCallStatus::InProgress => (None, None),
    };

    codex::ThreadItem::McpToolCall {
        id: tool.tool_call_key.clone(),
        server: "defra".to_string(),
        tool: tool.tool_name.clone(),
        status,
        arguments: parse_json_value(&tool.args).unwrap_or_else(|| json!({})),
        mcp_app_resource_uri: None,
        plugin_id: None,
        result,
        error,
        duration_ms: None,
    }
}

fn defra_tool_result_content(result: &str) -> Vec<Value> {
    preview_compact_text(result)
        .map(|text| vec![json!({ "type": "text", "text": text })])
        .unwrap_or_default()
}

fn defra_tool_call_status(tool: &DefraToolCallProgress) -> codex::McpToolCallStatus {
    let status = tool.status.trim().to_ascii_lowercase();
    if matches!(status.as_str(), "error" | "failed" | "failure" | "dead")
        || tool_result_looks_error(&tool.result)
    {
        return codex::McpToolCallStatus::Failed;
    }
    if matches!(
        status.as_str(),
        "completed" | "complete" | "success" | "succeeded"
    ) {
        return codex::McpToolCallStatus::Completed;
    }
    codex::McpToolCallStatus::InProgress
}

fn tool_result_looks_error(result: &str) -> bool {
    let trimmed = result.trim_start();
    trimmed.starts_with("Toolset error:") || trimmed.starts_with("JsonError:")
}

fn parse_json_value(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed)
        .ok()
        .or_else(|| Some(Value::String(trimmed.to_string())))
}

fn content_delta(previous: &str, current: &str) -> String {
    if current.is_empty() || previous == current {
        return String::new();
    }
    if let Some(delta) = current.strip_prefix(previous) {
        return delta.to_string();
    }
    let previous_trimmed_start = previous.trim_start();
    let current_trimmed_start = current.trim_start();
    if previous_trimmed_start == current_trimmed_start {
        return String::new();
    }
    if let Some(delta) = current_trimmed_start.strip_prefix(previous_trimmed_start) {
        return delta.to_string();
    }
    if previous.trim() == current.trim() {
        return String::new();
    }
    if previous.is_empty() {
        current.to_string()
    } else {
        format!("\n{current}")
    }
}

fn response_field_is_blank(response: &Value, field: &str) -> bool {
    response
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
}

fn terminal_turn_status(lifecycle_state: &str, response_status: &str) -> codex::TurnStatus {
    match (lifecycle_state, response_status) {
        ("interrupted" | "superseded", _) => codex::TurnStatus::Interrupted,
        ("failed" | "dead", _) | (_, "error") => codex::TurnStatus::Failed,
        _ => codex::TurnStatus::Completed,
    }
}

fn terminal_error_message(
    response_status: &str,
    response_error: Option<&str>,
    lifecycle_state: &str,
    failure_reason: &str,
) -> Option<String> {
    if let Some(error) = response_error
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(error.to_string());
    }
    if response_status == "error" {
        return Some("DEFRA response ended with status error".to_string());
    }
    if matches!(lifecycle_state, "failed" | "dead") {
        return Some(
            failure_reason
                .trim()
                .is_empty()
                .then(|| format!("DEFRA request ended with lifecycle_state {lifecycle_state}"))
                .unwrap_or_else(|| failure_reason.trim().to_string()),
        );
    }
    None
}

fn preview_compact_text(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = if trimmed.chars().count() > 120 {
        format!("{}...", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed.to_string()
    };
    Some(preview)
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
    notification: codex::ServerNotification,
) -> Result<()> {
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

fn trace_shim_event(message: impl AsRef<str>) {
    let Some(path) = std::env::var_os("DEFRA_CODEX_SHIM_TRACE") else {
        return;
    };
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", message.as_ref());
    }
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
