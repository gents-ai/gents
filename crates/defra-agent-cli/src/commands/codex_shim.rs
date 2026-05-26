use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use codex_app_server_protocol as codex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::cli::CodexShimArgs;
use crate::home_state::resolve_home_dir;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;

#[derive(Clone)]
struct ShimState {
    codex_home: PathBuf,
    cwd: PathBuf,
    model: Arc<str>,
    id_counter: Arc<AtomicU64>,
}

pub(crate) async fn codex_shim(args: CodexShimArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let codex_home = home_dir.join("codex-ui");
    fs::create_dir_all(&codex_home)
        .with_context(|| format!("creating Codex UI home {}", codex_home.display()))?;

    let state = ShimState {
        codex_home: codex_home.clone(),
        cwd: std::env::current_dir().context("resolving current working directory")?,
        model: Arc::from(args.model),
        id_counter: Arc::new(AtomicU64::new(1)),
    };

    let app = Router::new()
        .route("/", get(ws_upgrade))
        .with_state(state.clone());
    let addr = SocketAddr::new(args.bind_addr, args.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding Codex shim on {addr}"))?;

    println!("Codex TUI shim listening on ws://{addr}/");
    println!("Suggested launch:");
    println!(
        "  CODEX_HOME={} codex --dangerously-bypass-approvals-and-sandbox --remote ws://{addr}/",
        codex_home.display()
    );
    println!(
        "Note: stock Codex may run local onboarding before connecting when CODEX_HOME is empty."
    );

    axum::serve(listener, app)
        .await
        .context("serving Codex TUI shim")?;
    Ok(())
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<ShimState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: ShimState) {
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
                    account: None,
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
            let thread_id = state.next_id("defra-thread");
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
            start_echo_turn(
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
            start_echo_turn(
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

async fn start_echo_turn(
    socket: &mut WebSocket,
    state: &ShimState,
    request_id: codex::RequestId,
    thread_id: String,
    input: Vec<codex::UserInput>,
    start_response: bool,
) -> Result<()> {
    let turn_id = state.next_id("defra-turn");
    let item_id = state.next_id("defra-message");
    let user_text = user_text_from_input(&input);
    let response_text = if user_text.is_empty() {
        "DEFRA Codex shim is alive. GraphQL-backed turns will land in a follow-up.".to_string()
    } else {
        format!("DEFRA Codex shim echo: {user_text}")
    };
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

    let started_item = agent_message_item(&item_id, "");
    send_notification(
        socket,
        codex::ServerNotification::ItemStarted(codex::ItemStartedNotification {
            item: started_item,
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            started_at_ms: now_millis(),
        }),
    )
    .await?;
    send_notification(
        socket,
        codex::ServerNotification::AgentMessageDelta(codex::AgentMessageDeltaNotification {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            item_id: item_id.clone(),
            delta: response_text.clone(),
        }),
    )
    .await?;

    let completed_item = agent_message_item(&item_id, &response_text);
    send_notification(
        socket,
        codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
            item: completed_item.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            completed_at_ms: now_millis(),
        }),
    )
    .await?;
    send_notification(
        socket,
        codex::ServerNotification::TurnCompleted(codex::TurnCompletedNotification {
            thread_id,
            turn: turn_value(
                &turn_id,
                codex::TurnStatus::Completed,
                vec![completed_item],
                None,
            ),
        }),
    )
    .await
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

impl ShimState {
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
}
