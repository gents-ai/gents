use crate::error::BridgeError;
use tauri::State;

use crate::commands::{rename_session, send_chat_message};
use crate::snapshot::{
    apply_session_timeline_page_with_query, build_session_live_delta,
    build_session_snapshot_for_node_with_transcript,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    ChatSendRequest, ChatSendResult, DesktopSessionSnapshot, SessionLiveDeltaView,
    SessionRenameRequest,
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

/// The bounded session read behind `desktop_session_snapshot`.
pub(crate) async fn session_snapshot(
    core: &std::sync::Arc<gents_desktop_core::client::ClientCore>,
    session_id: String,
    node_did: Option<String>,
    request_id: Option<String>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<String>,
) -> Result<Option<DesktopSessionSnapshot>, BridgeError> {
    let started = std::time::Instant::now();
    let node_did = match node_did {
        Some(node_did) => Some(node_did),
        None => {
            let node_dids = core
                .store()
                .snapshot()
                .sessions
                .iter()
                .filter(|session| session.session_id == session_id)
                .map(|session| session.node_did.clone())
                .collect::<std::collections::BTreeSet<_>>();
            if node_dids.len() > 1 {
                return Err(BridgeError::untyped(format!(
                    "session {session_id} is ambiguous across multiple node owners"
                )));
            }
            node_dids.into_iter().next()
        }
    };
    if node_did.is_none() {
        return Err(BridgeError::untyped(format!(
            "session {session_id} has no resolved node owner"
        )));
    }
    let mut request_id = request_id;

    let hydrate_started = std::time::Instant::now();
    let mut hydration_observation = None;
    if let Some(node_did) = node_did.as_deref() {
        // Its transcript reads as empty here and a hydration request would be
        // refused, so the local header alone answers without any remote read.
        if core
            .session_unreadable_reason(&session_id, node_did)
            .is_some()
        {
            return Ok(build_session_snapshot_for_node_with_transcript(
                core.as_ref(),
                Some(node_did),
                &session_id,
                request_id.as_deref(),
                None,
                None,
                None,
                None,
                None,
                false,
                false,
                None,
            )
            .await);
        }
        match core
            .ensure_session_hydration_started(&session_id, node_did)
            .await
        {
            Ok(observation) => hydration_observation = Some(observation),
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::chat",
                    node_did,
                    session_id = %session_id,
                    error = %error,
                    "session hydration request failed; rendering whatever is already local"
                );
            }
        }
    }
    let hydrate_start_ms = hydrate_started.elapsed().as_millis() as u64;
    let transcript_started = std::time::Instant::now();
    let node_scope = node_did
        .as_deref()
        .and_then(|node_did| core.transcript_node_scope(node_did));
    let operator_access = node_did
        .as_deref()
        .and_then(|node_did| core.operator_graphql(node_did))
        .map(gents::config_client::ConfigAccess::Graphql);
    let requester_scope = {
        let store = core.store().snapshot();
        let session = node_did.as_deref().and_then(|node_did| {
            store
                .sessions
                .iter()
                .find(|row| row.session_id == session_id && row.node_did == node_did)
        });
        gents_desktop_core::client::session_transcript_requester_scope(
            session,
            node_did.as_deref(),
            node_scope.as_deref(),
            operator_access.is_some(),
        )
    };
    let page_read = async {
        match operator_access.as_ref() {
            Some(access) => {
                gents_desktop_core::client::load_session_transcript_page_on(
                    access,
                    &session_id,
                    node_did.as_deref(),
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
                    node_did.as_deref(),
                    requester_scope.as_deref(),
                    timeline_before_item_key.as_deref(),
                    timeline_limit,
                )
                .await
            }
        }
    };
    let transcript_page = page_read
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let page_request_doc_ids = transcript_page
        .store
        .transcript_messages
        .iter()
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    let fallback_request_id = match (request_id.is_none(), node_did.as_deref()) {
        (true, Some(node_did)) => core
            .store()
            .snapshot()
            .latest_request_id_for_session_for_node(&session_id, node_did),
        _ => None,
    };
    let refresh_started = std::time::Instant::now();
    let refreshed_projection = if let Some(node_did) = node_did.as_deref() {
        match core
            .refresh_local_request(
                node_did,
                &session_id,
                request_id.as_deref(),
                fallback_request_id.as_deref(),
                &page_request_doc_ids,
            )
            .await
        {
            Ok(Some(projection)) => Some(projection),
            Ok(None) => {
                return Err(BridgeError::untyped(
                    "selected request projection became stale; refresh is pending",
                ));
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::chat",
                    node_did,
                    request_id,
                    error = %error,
                    "selected request projection unavailable"
                );
                return Err(BridgeError::untyped(format!(
                    "selected request projection unavailable: {error}"
                )));
            }
        }
    } else {
        None
    };
    let request_refresh_ms = refresh_started.elapsed().as_millis() as u64;
    if request_id.is_none() {
        request_id = refreshed_projection
            .as_ref()
            .and_then(|projection| projection.selected_request_id.clone());
    }
    let fresh_requests = refreshed_projection
        .as_ref()
        .map(|projection| projection.requests.clone());
    let selected_requester_scope = refreshed_projection
        .as_ref()
        .map(|projection| projection.requester_did.clone())
        .unwrap_or_else(|| requester_scope.clone());
    let ownership_read = async {
        let requests = if let Some(requests) = fresh_requests.as_ref() {
            requests.clone()
        } else {
            let store = core.store().snapshot();
            store
                .requests
                .iter()
                .filter(|row| {
                    row.doc_id.is_some()
                        && row.session_id.as_deref() == Some(session_id.as_str())
                        && row.node_did.as_deref() == node_did.as_deref()
                        && row.requester_did.as_deref() == selected_requester_scope.as_deref()
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
    let context_read = async {
        if timeline_before_item_key.is_some() {
            return Ok(None);
        }
        let request = request_id.as_deref().and_then(|request_id| {
            if let Some(requests) = fresh_requests.as_ref() {
                gents_desktop_core::client::ClientStore::session_tip_request_from_rows(
                    requests,
                    &session_id,
                    node_did.as_deref(),
                    selected_requester_scope.as_deref(),
                    request_id,
                )
            } else {
                core.store().snapshot().session_tip_request(
                    &session_id,
                    node_did.as_deref(),
                    selected_requester_scope.as_deref(),
                    request_id,
                )
            }
        });
        let Some(request) = request else {
            return Ok(None);
        };
        match operator_access.as_ref() {
            Some(access) => gents_desktop_core::client::load_session_tip_store_on(access, &request)
                .await
                .map(Some),
            None => gents_desktop_core::client::load_session_tip_store(core.node(), &request)
                .await
                .map(Some),
        }
    };
    let (context, ownership) = tokio::join!(context_read, ownership_read);
    let context_store = match context {
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
    let mut snapshot = build_session_snapshot_for_node_with_transcript(
        core.as_ref(),
        node_did.as_deref(),
        &session_id,
        request_id.as_deref(),
        Some(&transcript_page.store),
        Some(&transcript_page.canonical_dependencies),
        context_store.as_ref(),
        prompt_ownership.as_ref(),
        hydration_observation,
        false,
        timeline_before_item_key.is_none(),
        fresh_requests.as_deref(),
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
        node_did: submitted.node_did,
        agent_id: submitted.agent_id,
    })
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
