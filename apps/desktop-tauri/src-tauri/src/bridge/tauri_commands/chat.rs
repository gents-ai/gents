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
