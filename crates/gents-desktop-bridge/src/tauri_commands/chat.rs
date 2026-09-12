use crate::error::BridgeError;
use tauri::State;

use crate::commands::{rename_session, send_chat_message};
use crate::snapshot::{
    apply_session_timeline_page_with_query, build_session_live_delta,
    build_session_snapshot_for_agent_with_transcript,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    ChatSendRequest, ChatSendResult, DesktopSessionSnapshot, SessionLiveDeltaView,
    SessionRenameRequest,
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
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .map(|session| session.agent_did.clone())
    });
    let request_id = request_id.or_else(|| {
        let store = core.store().snapshot();
        agent_did.as_deref().map_or_else(
            || store.latest_request_id_for_session(&session_id),
            |agent_did| store.latest_request_id_for_session_for_agent(&session_id, agent_did),
        )
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
    let operator_access = agent_did
        .as_deref()
        .and_then(|agent_did| core.operator_graphql(agent_did))
        .map(gents::config_client::ConfigAccess::Graphql);
    let page_read = async {
        match operator_access.as_ref() {
            Some(access) => {
                gents_desktop_core::client::load_session_transcript_page_on(
                    access,
                    &session_id,
                    agent_did.as_deref(),
                    requester_scope.as_deref(),
                    timeline_before_item_key.as_deref(),
                    timeline_limit,
                )
                .await
            }
            None => {
                gents_desktop_core::client::load_session_transcript_page(
                    core.node(),
                    &session_id,
                    agent_did.as_deref(),
                    requester_scope.as_deref(),
                    timeline_before_item_key.as_deref(),
                    timeline_limit,
                )
                .await
            }
        }
    };
    let (transcript_page, context_store) = if timeline_before_item_key.is_none() {
        let context_read = async {
            match operator_access.as_ref() {
                Some(access) => {
                    gents_desktop_core::client::load_session_context_store_on(
                        access,
                        &session_id,
                        agent_did.as_deref(),
                        requester_scope.as_deref(),
                    )
                    .await
                }
                None => {
                    gents_desktop_core::client::load_session_context_store(
                        core.node(),
                        &session_id,
                        agent_did.as_deref(),
                        requester_scope.as_deref(),
                    )
                    .await
                }
            }
        };
        let (page, context) = tokio::join!(page_read, context_read);
        let page = page.map_err(|error| BridgeError::untyped(error.to_string()))?;
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
                .map_err(|error| BridgeError::untyped(error.to_string()))?,
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
        .map_err(BridgeError::untyped)?;
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
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    let agent_did = agent_did
        .or_else(|| {
            core.store()
                .snapshot()
                .sessions
                .iter()
                .find(|session| session.session_id == session_id)
                .map(|session| session.agent_did.clone())
        })
        .ok_or_else(|| {
            BridgeError::untyped(
                "session hydration retry requires an agent for the selected session",
            )
        })?;
    core.retry_session_hydration(&session_id, &agent_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
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
    let agent_did = agent_did.or_else(|| {
        core.store()
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .map(|session| session.agent_did.clone())
    });
    // The live cursor is owned by the desktop replica. A local-standard
    // agent's operator GraphQL is a different DefraDB node, so a delta from
    // the replica can remain permanently "processing" after the agent has
    // committed its response and tool calls. Returning no delta promotes the
    // controller to its bounded full-session read, which refreshes the exact
    // request and transcript from the operator endpoint.
    if agent_did
        .as_deref()
        .is_some_and(|agent_did| core.operator_graphql(agent_did).is_some())
    {
        return Ok(None);
    }
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
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    send_chat_message(core.as_ref(), request)
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
    state: State<'_, DesktopAppState>,
) -> Result<RequestResendResultView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let submitted = core
        .resend_request(&request_id)
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
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let parent = core
        .store()
        .snapshot()
        .requests
        .iter()
        .find(|request| request.request_id == request_id)
        .cloned()
        .ok_or_else(|| {
            BridgeError::untyped(format!(
                "retry parent request not found: request_id={request_id}"
            ))
        })?;
    let submitted = core
        .retry_request(&parent)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
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
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let timeline = core
        .request_timeline(&agent_did, &request_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    serde_json::to_value(&timeline).map_err(|error| BridgeError::untyped(error.to_string()))
}
