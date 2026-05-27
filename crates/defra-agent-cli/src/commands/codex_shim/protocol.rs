use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use super::{trace, Outbound, ShimState};

pub(super) fn client_request_from_jsonrpc(
    request: codex::JSONRPCRequest,
) -> std::result::Result<codex::ClientRequest, serde_json::Error> {
    serde_json::from_value(serde_json::to_value(request)?)
}

pub(super) async fn send_typed_json_result<T>(
    outbound: &Outbound,
    id: codex::RequestId,
    value: Value,
) -> Result<()>
where
    T: DeserializeOwned + Serialize,
{
    let response = serde_json::from_value::<T>(value)
        .with_context(|| format!("validating Codex response {}", std::any::type_name::<T>()))?;
    send_result(outbound, id, response).await
}

pub(super) async fn send_result<T>(
    outbound: &Outbound,
    id: codex::RequestId,
    response: T,
) -> Result<()>
where
    T: Serialize,
{
    let result = serde_json::to_value(response).context("serializing Codex response payload")?;
    send_json(outbound, &codex::JSONRPCResponse { id, result }).await
}

pub(super) async fn send_error(
    outbound: &Outbound,
    id: codex::RequestId,
    code: i64,
    message: String,
) -> Result<()> {
    send_json(
        outbound,
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

pub(super) async fn send_notification(
    outbound: &Outbound,
    state: &ShimState,
    notification: codex::ServerNotification,
) -> Result<()> {
    trace::codex_notification(&state.trace_path, &notification);
    send_json(outbound, &notification).await
}

async fn send_json<T>(outbound: &Outbound, value: &T) -> Result<()>
where
    T: Serialize,
{
    let text = serde_json::to_string(value).context("serializing Codex shim WebSocket message")?;
    outbound
        .send(text)
        .map_err(|_| anyhow::anyhow!("Codex shim WebSocket writer closed"))
}

pub(super) fn initialize_result(state: &ShimState) -> Value {
    json!({
        "userAgent": concat!("defra-agent-codex-shim/", env!("CARGO_PKG_VERSION")),
        "codexHome": absolute_path(&state.codex_home),
        "platformFamily": std::env::consts::FAMILY,
        "platformOs": std::env::consts::OS
    })
}

pub(super) fn model_summary(state: &ShimState) -> Value {
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

pub(super) fn thread_json(
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

pub(super) fn turn_value(
    turn_id: &str,
    status: codex::TurnStatus,
    items: Vec<codex::ThreadItem>,
    error: Option<codex::TurnError>,
) -> codex::Turn {
    let now = now_seconds();
    let completed_at = (!matches!(status, codex::TurnStatus::InProgress)).then_some(now);
    let items_view = if items.is_empty() {
        codex::TurnItemsView::NotLoaded
    } else {
        codex::TurnItemsView::Full
    };
    codex::Turn {
        id: turn_id.to_string(),
        items,
        items_view,
        status,
        error,
        started_at: Some(now),
        completed_at,
        duration_ms: None,
    }
}

pub(super) fn agent_message_item(item_id: &str, text: &str) -> codex::ThreadItem {
    codex::ThreadItem::AgentMessage {
        id: item_id.to_string(),
        text: text.to_string(),
        phase: None,
        memory_citation: None,
    }
}

pub(super) async fn send_committed_user_message(
    outbound: &Outbound,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
    input: &[codex::UserInput],
) -> Result<()> {
    send_notification(
        outbound,
        state,
        codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
            item: codex::ThreadItem::UserMessage {
                id: state.next_id("defra-user-message"),
                content: input.to_vec(),
            },
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            completed_at_ms: now_millis(),
        }),
    )
    .await
}

pub(super) fn empty_rate_limits() -> codex::RateLimitSnapshot {
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

pub(super) fn user_text_from_input(input: &[codex::UserInput]) -> String {
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

pub(super) fn codex_turn_metadata(cwd: &Path) -> String {
    json!({
        "codex_shim": {
            "cwd": absolute_path(cwd)
        }
    })
    .to_string()
}

pub(super) fn codex_steering_metadata(cwd: &Path, queued_after_request_id: &str) -> String {
    json!({
        "codex_shim": {
            "cwd": absolute_path(cwd)
        },
        "queue": {
            "source": "steering",
            "policy": "append",
            "key": null,
            "queued_after_request_id": queued_after_request_id
        }
    })
    .to_string()
}

pub(super) fn effective_cwd(state: &ShimState, cwd: Option<&str>) -> PathBuf {
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

pub(super) fn absolute_path(path: &Path) -> String {
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

pub(super) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
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
