use crate::error::BridgeError;
use tauri::State;

use crate::commands::{rename_conversation, send_chat_message};
use crate::snapshot::{
    apply_session_timeline_page_with_query, build_session_live_delta,
    build_session_snapshot_for_agent_with_transcript,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    ChatSendRequest, ChatSendResult, ConversationRenameRequest, DesktopSessionSnapshot,
    SessionLiveDeltaView,
};

#[tauri::command]
pub async fn desktop_session_snapshot(
    session_id: String,
    agent_did: Option<String>,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopSessionSnapshot>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    let agent_did = agent_did.or_else(|| {
        core.store()
            .snapshot()
            .conversations
            .iter()
            .find(|conversation| conversation.session_id == session_id)
            .and_then(|conversation| conversation.agent_did.clone())
    });

    if let Some(agent_did) = agent_did.as_deref() {
        if let Err(error) = core
            .ensure_session_hydration_started(&session_id, agent_did)
            .await
        {
            tracing::warn!(
                target: "gents_desktop::chat",
                agent_did,
                session_id = %session_id,
                error = %error,
                "session hydration request failed; rendering whatever is already local"
            );
        }
    }
    if let (Some(agent_did), Some(request_id)) = (agent_did.as_deref(), request_id.as_deref()) {
        if let Err(error) = core.refresh_local_request(agent_did, request_id).await {
            tracing::warn!(
                target: "gents_desktop::chat",
                agent_did,
                request_id,
                error = %error,
                "selected local request refresh failed; returning the last observed session"
            );
        }
    }
    let requester_scope = if let Some(agent_did) = agent_did.as_deref() {
        core.peer_records()
            .await
            .iter()
            .any(|peer| peer.agent_did == agent_did && peer.is_enrollment())
            .then(|| core.principal().did().to_string())
    } else {
        None
    };
    let page_read = gents_desktop_core::client::load_session_transcript_page(
        core.node(),
        &session_id,
        agent_did.as_deref(),
        requester_scope.as_deref(),
        timeline_before_item_key.as_deref(),
        timeline_limit,
    );
    let (transcript_page, context_store) = if timeline_before_item_key.is_none() {
        let context_read = gents_desktop_core::client::load_session_context_store(
            core.node(),
            &session_id,
            agent_did.as_deref(),
            requester_scope.as_deref(),
        );
        let (page, context) = tokio::join!(page_read, context_read);
        let page = page.map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
        let context = match context {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::chat",
                    session_id,
                    error = %error,
                    "session context query failed; returning the bounded transcript with inexact totals"
                );
                None
            }
        };
        (page, context)
    } else {
        (
            page_read
                .await
                .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?,
            None,
        )
    };
    let mut snapshot = build_session_snapshot_for_agent_with_transcript(
        core.as_ref(),
        agent_did.as_deref(),
        &session_id,
        request_id.as_deref(),
        Some(&transcript_page.store),
        context_store.as_ref(),
        context_store.is_some(),
        timeline_before_item_key.is_none(),
    )
    .await;
    if let Some(snapshot) = snapshot.as_mut() {
        apply_session_timeline_page_with_query(
            snapshot,
            timeline_before_item_key.as_deref(),
            timeline_limit,
            Some(&transcript_page),
        )
        .map_err(BridgeError::from_legacy_message)?;
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn desktop_session_hydration_retry(
    session_id: String,
    agent_did: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };
    let agent_did = agent_did
        .or_else(|| {
            core.store()
                .snapshot()
                .conversations
                .iter()
                .find(|conversation| conversation.session_id == session_id)
                .and_then(|conversation| conversation.agent_did.clone())
        })
        .ok_or_else(|| {
            BridgeError::from_legacy_message(
                "session hydration retry requires an agent for the selected session",
            )
        })?;
    core.retry_session_hydration(&session_id, &agent_did)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn desktop_session_live_delta(
    session_id: String,
    agent_did: Option<String>,
    request_id: String,
    base_reconcile_version: u64,
    base_content_byte_len: usize,
    base_content_hash: String,
    base_reasoning_byte_len: usize,
    base_reasoning_hash: String,
    state: State<'_, DesktopAppState>,
) -> Result<Option<SessionLiveDeltaView>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    Ok(Some(build_session_live_delta(
        core.as_ref(),
        &session_id,
        agent_did.as_deref(),
        &request_id,
        base_reconcile_version,
        base_content_byte_len,
        &base_content_hash,
        base_reasoning_byte_len,
        &base_reasoning_hash,
    )))
}

#[tauri::command]
pub async fn desktop_chat_send(
    request: ChatSendRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    send_chat_message(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

#[tauri::command]
pub async fn desktop_conversation_rename(
    request: ConversationRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    rename_conversation(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
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
    state: State<'_, DesktopAppState>,
) -> Result<RequestResendResultView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    let submitted = core
        .resend_request(&request_id)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    Ok(RequestResendResultView {
        request_id: submitted.request_id,
        session_id: submitted.session_id,
    })
}

#[tauri::command]
pub async fn desktop_request_retry(
    request_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    let parent = core
        .store()
        .snapshot()
        .requests
        .iter()
        .find(|request| request.request_id == request_id)
        .cloned()
        .ok_or_else(|| {
            BridgeError::from_legacy_message(format!(
                "retry parent request not found: request_id={request_id}"
            ))
        })?;
    let submitted = core
        .retry_request(&parent)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    Ok(ChatSendResult {
        session_id: submitted.session_id,
        request_id: submitted.request_id,
        agent_did: submitted.agent_did,
        behavior_id: submitted.behavior_id,
    })
}

#[tauri::command]
pub async fn desktop_request_timeline(
    agent_did: String,
    request_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<serde_json::Value, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    let timeline = core
        .request_timeline(&agent_did, &request_id)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    serde_json::to_value(&timeline)
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}
