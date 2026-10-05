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
    let started = std::time::Instant::now();
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

    let hydrate_started = std::time::Instant::now();
    if let Some(agent_did) = agent_did.as_deref() {
        // Its transcript reads as empty here and a hydration request would be
        // refused, so the local header alone answers without any remote read.
        if core
            .session_unreadable_reason(&session_id, agent_did)
            .is_some()
        {
            return Ok(build_session_snapshot_for_agent_with_transcript(
                core.as_ref(),
                Some(agent_did),
                &session_id,
                request_id.as_deref(),
                None,
                None,
                None,
                None,
                false,
                false,
            )
            .await);
        }
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
    let hydrate_start_ms = hydrate_started.elapsed().as_millis() as u64;
    let refresh_started = std::time::Instant::now();
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
    let request_refresh_ms = refresh_started.elapsed().as_millis() as u64;
    let transcript_started = std::time::Instant::now();
    let principal_scope = agent_did
        .as_deref()
        .and_then(|agent_did| core.transcript_principal_scope(agent_did));
    let operator_access = agent_did
        .as_deref()
        .and_then(|agent_did| core.operator_graphql(agent_did))
        .map(gents::config_client::ConfigAccess::Graphql);
    let requester_scope = {
        let store = core.store().snapshot();
        let session = agent_did.as_deref().and_then(|agent_did| {
            store
                .sessions
                .iter()
                .find(|row| row.session_id == session_id && row.agent_did == agent_did)
        });
        gents_desktop_core::client::session_transcript_requester_scope(
            session,
            agent_did.as_deref(),
            principal_scope.as_deref(),
            operator_access.is_some(),
        )
    };
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
    let ownership_read = async {
        let requests = {
            let store = core.store().snapshot();
            store
                .requests
                .iter()
                .filter(|row| {
                    row.doc_id.is_some()
                        && row.session_id.as_deref() == Some(session_id.as_str())
                        && row.agent_did.as_deref() == agent_did.as_deref()
                        && row.requester_did.as_deref() == requester_scope.as_deref()
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        match operator_access.as_ref() {
            Some(access) => {
                gents_desktop_core::client::load_request_prompt_ownership_on(access, &requests)
                    .await
            }
            None => {
                gents_desktop_core::client::load_request_prompt_ownership(core.node(), &requests)
                    .await
            }
        }
    };
    let page_and_tip = async {
        let result = if timeline_before_item_key.is_none() {
            let context_read = async {
                let request = {
                    let store = core.store().snapshot();
                    store
                        .requests
                        .iter()
                        .find(|row| {
                            Some(row.request_id.as_str()) == request_id.as_deref()
                                && row.session_id.as_deref() == Some(session_id.as_str())
                                && row.agent_did.as_deref() == agent_did.as_deref()
                                && row.requester_did.as_deref() == requester_scope.as_deref()
                        })
                        .cloned()
                };
                let Some(request) = request else {
                    return Ok(None);
                };
                match operator_access.as_ref() {
                    Some(access) => {
                        gents_desktop_core::client::load_session_tip_store_on(access, &request)
                            .await
                            .map(Some)
                    }
                    None => {
                        gents_desktop_core::client::load_session_tip_store(core.node(), &request)
                            .await
                            .map(Some)
                    }
                }
            };
            let (page, context) = tokio::join!(page_read, context_read);
            let page = page.map_err(|error| BridgeError::untyped(error.to_string()))?;
            let context = match context {
                Ok(store) => store,
                Err(error) => {
                    tracing::warn!(
                        target: "gents_desktop::chat",
                        session_id,
                        error = %error,
                        "session tip query failed; returning the bounded transcript without live ownership evidence"
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
        Ok::<_, BridgeError>(result)
    };
    let (page_and_tip, ownership) = tokio::join!(page_and_tip, ownership_read);
    let (transcript_page, context_store) = page_and_tip?;
    let prompt_ownership = match ownership {
        Ok(facts) => Some(facts),
        Err(error) => {
            tracing::warn!(session_id, error = %error, "request prompt ownership unavailable; suppressing unproven pending inputs");
            None
        }
    };
    let transcript_page_tip_ms = transcript_started.elapsed().as_millis() as u64;
    let projection_started = std::time::Instant::now();
    let context_store = context_store.map(|tip| transcript_page.store.merge_snapshot(tip));
    let mut snapshot = build_session_snapshot_for_agent_with_transcript(
        core.as_ref(),
        agent_did.as_deref(),
        &session_id,
        request_id.as_deref(),
        Some(&transcript_page.store),
        Some(&transcript_page.canonical_dependencies),
        context_store.as_ref(),
        prompt_ownership.as_ref(),
        false,
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
    let projection_ms = projection_started.elapsed().as_millis() as u64;
    let elapsed = started.elapsed();
    if elapsed > std::time::Duration::from_secs(1) {
        tracing::info!(
            target: "gents_desktop::chat",
            session_id,
            hydrate_start_ms,
            request_refresh_ms,
            transcript_page_tip_ms,
            projection_ms,
            elapsed_ms = elapsed.as_millis() as u64,
            "loaded slow desktop session snapshot"
        );
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
    agent_did: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<RequestResendResultView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let scope = agent_did.or_else(|| core.selected_agent_did());
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
    agent_did: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let scope = agent_did.or_else(|| core.selected_agent_did());
    let parent = core
        .request_in_scope(&request_id, scope.as_deref())
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
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
