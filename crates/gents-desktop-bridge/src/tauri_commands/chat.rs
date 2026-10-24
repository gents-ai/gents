use crate::error::BridgeError;
use tauri::State;

use crate::commands::{rename_session, send_chat_message};

#[tauri::command]
pub async fn desktop_session_goal(
    node_did: String,
    session_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Option<crate::types::SessionGoalView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    crate::commands::session_goal(&core, &node_did, &session_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_goal_control(
    request: crate::types::GoalControlRequest,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::GoalControlResult, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    crate::commands::control_goal(&core, request).await
}
use crate::snapshot::build_session_live_delta;
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    ChatSendRequest, ChatSendResult, DesktopSessionSnapshot, SessionLiveDeltaView,
    SessionRenameRequest, SessionRowsView, SessionRowsWindow,
};

#[tauri::command]
pub async fn desktop_session_snapshot(
    session_id: String,
    node_did: Option<String>,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopSessionSnapshot>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    session_snapshot(
        &core,
        session_id,
        node_did,
        request_id,
        timeline_limit,
        timeline_before_item_key,
    )
    .await
}

pub(crate) use crate::commands::session_snapshot;

#[tauri::command]
pub async fn desktop_session_rows(
    session_id: String,
    node_did: Option<String>,
    window: SessionRowsWindow,
    limit: Option<usize>,
    state: State<'_, DesktopAppState>,
) -> Result<Option<SessionRowsView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    crate::commands::session_rows(&core, session_id, node_did, window, limit)
        .await
        .map(Some)
}

#[tauri::command]
pub async fn desktop_session_hydration_retry(
    session_id: String,
    node_did: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    let node_did = node_did
        .or_else(|| {
            core.store()
                .snapshot()
                .sessions
                .iter()
                .find(|session| session.session_id == session_id)
                .map(|session| session.node_did.clone())
        })
        .ok_or_else(|| {
            BridgeError::untyped("session hydration retry requires a node for the selected session")
        })?;
    core.retry_session_hydration(&session_id, &node_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn desktop_session_live_delta(
    session_id: String,
    node_did: Option<String>,
    request_id: String,
    base_live_cursor: String,
    base_content_byte_len: usize,
    base_content_hash: String,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: String,
    state: State<'_, DesktopAppState>,
) -> Result<Option<SessionLiveDeltaView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    let node_did = node_did.or_else(|| {
        core.store()
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .map(|session| session.node_did.clone())
    });
    Ok(Some(
        build_session_live_delta(
            core.as_ref(),
            &session_id,
            node_did.as_deref(),
            &request_id,
            &base_live_cursor,
            base_content_byte_len,
            &base_content_hash,
            base_reasoning_byte_len,
            &base_reasoning_hash,
        )
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?,
    ))
}

#[tauri::command]
pub async fn desktop_chat_send(
    request: ChatSendRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    send_chat_message(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_chat_submission_recover(
    query: crate::types::ChatSubmissionQuery,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::ChatSubmissionRecovery, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    crate::commands::recover_chat_submission(&core, query)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_chat_submission_acknowledge(
    receipt: crate::types::ChatSubmissionReceipt,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    crate::commands::acknowledge_chat_submission(&core, receipt)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_session_fork_before_message(
    request: crate::types::SessionForkBeforeRequest,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::SessionForkResult, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    crate::commands::fork_session_before_message(&core, request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_session_rename(
    request: SessionRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    rename_session(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestResendResultView {
    pub request_id: String,
    pub session_id: String,
}

#[tauri::command]
pub async fn desktop_request_resend(
    request_id: String,
    node_did: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<RequestResendResultView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let scope = node_did.or_else(|| core.selected_node_did());
    let submitted = core
        .resend_request_in_scope(&request_id, scope.as_deref())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    Ok(RequestResendResultView {
        request_id: submitted.request_id,
        session_id: submitted.session_id,
    })
}

#[tauri::command]
pub async fn desktop_request_retry(
    request_id: String,
    node_did: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let scope = node_did.or_else(|| core.selected_node_did());
    crate::commands::retry_request(&core, &request_id, scope.as_deref())
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_request_timeline(
    node_did: String,
    request_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::RequestTimelineView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let timeline = core
        .request_timeline(&node_did, &request_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let value =
        serde_json::to_value(&timeline).map_err(|error| BridgeError::untyped(error.to_string()))?;
    serde_json::from_value(value).map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_pending_queue_edit(
    request: crate::types::PendingQueueEditRequest,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::PendingQueueEditResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    crate::commands::edit_pending_queue(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_pending_message_steer(
    request: crate::types::PendingMessageSteerRequest,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::PendingQueueEditResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    crate::commands::steer_pending_message(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
