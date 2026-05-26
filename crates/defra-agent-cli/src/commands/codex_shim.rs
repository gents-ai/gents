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
use serde_json::{json, Value};
use tokio::net::TcpListener;

use crate::cli::CodexShimArgs;
use crate::home_state::resolve_home_dir;

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

        let Ok(payload) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(method) = payload.get("method").and_then(Value::as_str) else {
            continue;
        };

        if payload.get("id").is_none() {
            continue;
        }

        if let Err(err) = handle_request(&mut socket, &state, &payload, method).await {
            tracing::warn!(%err, method, "Codex shim request handling failed");
            return;
        }
    }
}

async fn handle_request(
    socket: &mut WebSocket,
    state: &ShimState,
    payload: &Value,
    method: &str,
) -> Result<()> {
    let id = payload
        .get("id")
        .cloned()
        .context("JSON-RPC request missing id")?;
    match method {
        "initialize" => send_result(socket, id, initialize_result(state)).await,
        "account/read" => {
            send_result(
                socket,
                id,
                json!({
                    "account": null,
                    "requiresOpenaiAuth": false
                }),
            )
            .await
        }
        "account/rateLimits/read" => {
            send_result(
                socket,
                id,
                json!({
                    "rateLimits": empty_rate_limits(),
                    "rateLimitsByLimitId": null
                }),
            )
            .await
        }
        "model/list" => {
            send_result(
                socket,
                id,
                json!({
                    "data": [model_summary(state)],
                    "nextCursor": null
                }),
            )
            .await
        }
        "modelProvider/capabilities/read" => {
            send_result(
                socket,
                id,
                json!({
                    "namespaceTools": false,
                    "imageGeneration": false,
                    "webSearch": false
                }),
            )
            .await
        }
        "config/read" => {
            send_result(
                socket,
                id,
                json!({
                    "config": {
                        "model": state.model.as_ref(),
                        "modelProvider": "defra",
                        "approvalPolicy": "never",
                        "sandboxMode": "danger-full-access"
                    },
                    "origins": {}
                }),
            )
            .await
        }
        "config/batchWrite" | "config/value/write" => {
            send_result(
                socket,
                id,
                json!({
                    "status": "ok",
                    "version": "defra-shim",
                    "filePath": absolute_path(&state.codex_home.join("config.toml")),
                    "overriddenMetadata": null
                }),
            )
            .await
        }
        "configRequirements/read" => send_result(socket, id, json!({ "requirements": null })).await,
        "externalAgentConfig/detect" => send_result(socket, id, json!({ "items": [] })).await,
        "externalAgentConfig/import" => send_result(socket, id, json!({})).await,
        "experimentalFeature/list" => {
            send_result(socket, id, json!({ "data": [], "nextCursor": null })).await
        }
        "permissionProfile/list" => {
            send_result(socket, id, json!({ "data": [], "nextCursor": null })).await
        }
        "collaborationMode/list" => send_result(socket, id, json!({ "data": [] })).await,
        "skills/list" | "hooks/list" => send_result(socket, id, json!({ "data": [] })).await,
        "plugin/list" => {
            send_result(
                socket,
                id,
                json!({
                    "marketplaces": [],
                    "marketplaceLoadErrors": [],
                    "featuredPluginIds": []
                }),
            )
            .await
        }
        "mcpServerStatus/list" => {
            send_result(socket, id, json!({ "data": [], "nextCursor": null })).await
        }
        "thread/start" => {
            let thread_id = state.next_id("defra-thread");
            let thread = thread_value(state, &thread_id, None, "idle", Vec::new());
            send_result(
                socket,
                id,
                json!({
                    "thread": thread,
                    "model": state.model.as_ref(),
                    "modelProvider": "defra",
                    "serviceTier": null,
                    "cwd": absolute_path(&state.cwd),
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
        "thread/list" => {
            send_result(
                socket,
                id,
                json!({ "data": [], "nextCursor": null, "backwardsCursor": null }),
            )
            .await
        }
        "thread/loaded/list" => {
            send_result(socket, id, json!({ "data": [], "nextCursor": null })).await
        }
        "thread/read" => {
            let thread_id = payload
                .pointer("/params/threadId")
                .and_then(Value::as_str)
                .unwrap_or("defra-thread");
            send_result(
                socket,
                id,
                json!({ "thread": thread_value(state, thread_id, None, "idle", Vec::new()) }),
            )
            .await
        }
        "thread/unsubscribe" => send_result(socket, id, json!({ "status": "unsubscribed" })).await,
        "turn/start" | "turn/steer" => start_echo_turn(socket, state, payload, id).await,
        "turn/interrupt" => send_result(socket, id, json!({})).await,
        _ => {
            send_error(
                socket,
                id,
                -32601,
                format!("unsupported Codex shim method `{method}`"),
            )
            .await
        }
    }
}

async fn start_echo_turn(
    socket: &mut WebSocket,
    state: &ShimState,
    payload: &Value,
    id: Value,
) -> Result<()> {
    let thread_id = payload
        .pointer("/params/threadId")
        .and_then(Value::as_str)
        .unwrap_or("defra-thread");
    let turn_id = state.next_id("defra-turn");
    let item_id = state.next_id("defra-message");
    let user_text = user_text_from_turn(payload);
    let response_text = if user_text.is_empty() {
        "DEFRA Codex shim is alive. GraphQL-backed turns will land in a follow-up."
    } else {
        "DEFRA Codex shim echo: "
    };
    let response_text = if user_text.is_empty() {
        response_text.to_string()
    } else {
        format!("{response_text}{user_text}")
    };
    let started_turn = turn_value(&turn_id, "inProgress", Vec::new(), None);

    send_result(socket, id, json!({ "turn": started_turn.clone() })).await?;
    send_notification(
        socket,
        "turn/started",
        json!({
            "threadId": thread_id,
            "turn": started_turn
        }),
    )
    .await?;

    let started_item = agent_message_item(&item_id, "");
    send_notification(
        socket,
        "item/started",
        json!({
            "item": started_item,
            "threadId": thread_id,
            "turnId": turn_id,
            "startedAtMs": now_millis()
        }),
    )
    .await?;
    send_notification(
        socket,
        "item/agentMessage/delta",
        json!({
            "threadId": thread_id,
            "turnId": turn_id,
            "itemId": item_id,
            "delta": response_text
        }),
    )
    .await?;

    let completed_item = agent_message_item(&item_id, &response_text);
    send_notification(
        socket,
        "item/completed",
        json!({
            "item": completed_item.clone(),
            "threadId": thread_id,
            "turnId": turn_id,
            "completedAtMs": now_millis()
        }),
    )
    .await?;
    send_notification(
        socket,
        "turn/completed",
        json!({
            "threadId": thread_id,
            "turn": turn_value(&turn_id, "completed", vec![completed_item], None)
        }),
    )
    .await
}

async fn send_result(socket: &mut WebSocket, id: Value, result: Value) -> Result<()> {
    send_json(socket, json!({ "id": id, "result": result })).await
}

async fn send_error(socket: &mut WebSocket, id: Value, code: i64, message: String) -> Result<()> {
    send_json(
        socket,
        json!({
            "id": id,
            "error": {
                "code": code,
                "message": message
            }
        }),
    )
    .await
}

async fn send_notification(socket: &mut WebSocket, method: &str, params: Value) -> Result<()> {
    send_json(socket, json!({ "method": method, "params": params })).await
}

async fn send_json(socket: &mut WebSocket, value: Value) -> Result<()> {
    socket
        .send(Message::Text(value.to_string().into()))
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

fn thread_value(
    state: &ShimState,
    thread_id: &str,
    preview: Option<&str>,
    status: &str,
    turns: Vec<Value>,
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
        "status": { "type": status },
        "path": null,
        "cwd": absolute_path(&state.cwd),
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

fn turn_value(turn_id: &str, status: &str, items: Vec<Value>, error: Option<Value>) -> Value {
    let now = now_seconds();
    let completed_at = (status != "inProgress").then_some(now);
    json!({
        "id": turn_id,
        "items": items,
        "itemsView": "full",
        "status": status,
        "error": error,
        "startedAt": now,
        "completedAt": completed_at,
        "durationMs": null
    })
}

fn agent_message_item(item_id: &str, text: &str) -> Value {
    json!({
        "type": "agentMessage",
        "id": item_id,
        "text": text,
        "phase": null,
        "memoryCitation": null
    })
}

fn empty_rate_limits() -> Value {
    json!({
        "limitId": null,
        "limitName": null,
        "primary": null,
        "secondary": null,
        "credits": null,
        "planType": null,
        "rateLimitReachedType": null
    })
}

fn user_text_from_turn(payload: &Value) -> String {
    payload
        .pointer("/params/input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    (item.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| item.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
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
        let payload = json!({
            "params": {
                "input": [
                    { "type": "text", "text": "hello", "textElements": [] },
                    { "type": "image", "url": "https://example.invalid/image.png" },
                    { "type": "text", "text": "world", "textElements": [] }
                ]
            }
        });

        assert_eq!(user_text_from_turn(&payload), "hello\nworld");
    }

    #[test]
    fn thread_status_uses_codex_tag_shape() {
        let state = ShimState {
            codex_home: PathBuf::from("/tmp/defra-codex-ui"),
            cwd: PathBuf::from("/tmp"),
            model: Arc::from("defra-default"),
            id_counter: Arc::new(AtomicU64::new(1)),
        };

        let thread = thread_value(&state, "thread-1", Some("preview"), "idle", Vec::new());
        assert_eq!(thread.pointer("/status/type"), Some(&json!("idle")));
        assert_eq!(thread.pointer("/source"), Some(&json!("cli")));
    }
}
