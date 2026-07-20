use tauri::State;

use super::super::commands::{rename_conversation, send_chat_message};
use super::super::snapshot::build_session_snapshot_from_store_for_agent;
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    ChatSendRequest, ChatSendResult, ConversationRenameRequest, DesktopSessionSnapshot,
};

#[tauri::command]
pub(crate) fn desktop_session_snapshot(
    session_id: String,
    agent_did: Option<String>,
    request_id: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopSessionSnapshot>, String> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };

    let snapshot = tauri::async_runtime::block_on(async move { core.store().snapshot() });
    Ok(build_session_snapshot_from_store_for_agent(
        snapshot.as_ref(),
        agent_did.as_deref(),
        &session_id,
        request_id.as_deref(),
    ))
}

#[tauri::command]
pub(crate) fn desktop_chat_send(
    request: ChatSendRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        send_chat_message(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub(crate) fn desktop_conversation_rename(
    request: ConversationRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        rename_conversation(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())
    })
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionForkResultView {
    pub session_id: String,
    pub copied_messages: u32,
    pub copied_tool_calls: u32,
}

#[tauri::command]
pub(crate) fn desktop_session_fork(
    agent_did: String,
    session_id: String,
    at_user_turn: u32,
    behavior_id: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<SessionForkResultView, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        let outcome = core
            .fork_session(
                &agent_did,
                &session_id,
                at_user_turn,
                behavior_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(SessionForkResultView {
            session_id: outcome.session_id,
            copied_messages: outcome.copied_messages,
            copied_tool_calls: outcome.copied_tool_calls,
        })
    })
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestResendResultView {
    pub request_id: String,
    pub session_id: String,
}

#[tauri::command]
pub(crate) fn desktop_request_resend(
    request_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<RequestResendResultView, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        let submitted = core
            .resend_request(&request_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(RequestResendResultView {
            request_id: submitted.request_id,
            session_id: submitted.session_id,
        })
    })
}

#[tauri::command]
pub(crate) fn desktop_request_timeline(
    agent_did: String,
    request_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<serde_json::Value, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        let timeline = core
            .request_timeline(&agent_did, &request_id)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(&timeline).map_err(|error| error.to_string())
    })
}
