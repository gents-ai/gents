use std::time::Duration;

use gents_desktop_core::client::ClientCore;
use serde::Serialize;

use crate::bridge::snapshot::{
    build_runtime_snapshot, build_session_snapshot_from_store_for_agent,
};
use crate::bridge::types::{turn_state_label, DesktopClientSnapshot, DesktopSessionSnapshot};
use crate::live_fixture::LiveBridgeFixture;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestRowDiagnostics {
    status: Option<String>,
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
    created_at: Option<String>,
    claimed_at: Option<String>,
    interrupt_requested_at: Option<String>,
    valid_until: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResponseRowDiagnostics {
    status: Option<String>,
    error_message: Option<String>,
    progress_seq: Option<i64>,
    materialized_message_sequence: Option<i64>,
    materialized_at: Option<String>,
    completed_at: Option<String>,
    content_len: usize,
    reasoning_len: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolCallDiagnostics {
    total: usize,
    completed: usize,
    pending: usize,
    latest_tool_name: Option<String>,
    latest_status: Option<String>,
    latest_completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestDiagnostics {
    source: String,
    session_id: String,
    request_id: String,
    refresh_error: Option<String>,
    turn_state: Option<String>,
    latest_request_id: Option<String>,
    conversation_updated_at: Option<String>,
    request: Option<RequestRowDiagnostics>,
    response: Option<ResponseRowDiagnostics>,
    matching_response_count: usize,
    matching_response_progress_seqs: Vec<i64>,
    matching_response_statuses: Vec<String>,
    tool_calls: ToolCallDiagnostics,
    tool_result_count: usize,
    message_count: usize,
    timeline_count: usize,
    active_response_overlay_content_len: usize,
    active_response_overlay_reasoning_len: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestDiagnosticsBundle {
    desktop: RequestDiagnostics,
    remote: RequestDiagnostics,
}

pub(crate) async fn build_desktop_client_snapshot(
    fixture: &LiveBridgeFixture,
) -> DesktopClientSnapshot {
    let _ = refresh_store_with_timeout(fixture.desktop_core().as_ref()).await;
    DesktopClientSnapshot {
        bootstrap: fixture.build_bootstrap_summary().await,
        client: Some(build_runtime_snapshot(fixture.desktop_core().as_ref()).await),
    }
}

pub(crate) async fn build_desktop_session_snapshot(
    fixture: &LiveBridgeFixture,
    agent_did: Option<&str>,
    session_id: &str,
    request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let _ = refresh_store_with_timeout(fixture.desktop_core().as_ref()).await;
    let snapshot = fixture.desktop_core().store().snapshot();
    build_session_snapshot_from_store_for_agent(
        snapshot.as_ref(),
        agent_did,
        session_id,
        request_id,
    )
}

pub(crate) async fn build_request_diagnostics_bundle(
    fixture: &LiveBridgeFixture,
    session_id: &str,
    request_id: &str,
) -> RequestDiagnosticsBundle {
    RequestDiagnosticsBundle {
        desktop: build_request_diagnostics(
            "desktop",
            fixture.desktop_core().as_ref(),
            session_id,
            request_id,
        )
        .await,
        remote: build_request_diagnostics(
            "remote",
            fixture.remote_core().as_ref(),
            session_id,
            request_id,
        )
        .await,
    }
}

async fn build_request_diagnostics(
    source: &str,
    core: &ClientCore,
    session_id: &str,
    request_id: &str,
) -> RequestDiagnostics {
    let refresh_error = refresh_store_with_timeout(core).await;
    let snapshot = core.store().snapshot();
    let request = snapshot
        .requests
        .iter()
        .find(|row| row.request_id == request_id)
        .cloned();
    let matching_responses = snapshot
        .responses
        .iter()
        .filter(|row| row.request_id.as_deref() == Some(request_id))
        .collect::<Vec<_>>();
    let response = snapshot.latest_response_for_request(request_id).cloned();
    let transcript = snapshot.transcript(session_id);
    let relevant_tool_calls = transcript.tool_calls;
    let latest_tool_call = relevant_tool_calls.last().copied();
    let completed_tool_calls = relevant_tool_calls
        .iter()
        .filter(|row| {
            row.completed_at
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || row
                    .status
                    .as_deref()
                    .is_some_and(|value| matches!(value, "completed" | "success" | "ok"))
        })
        .count();
    let session_snapshot = build_session_snapshot_from_store_for_agent(
        snapshot.as_ref(),
        None,
        session_id,
        Some(request_id),
    );

    RequestDiagnostics {
        source: source.to_string(),
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        refresh_error,
        turn_state: snapshot
            .derive_turn_for_request(request_id)
            .or_else(|| snapshot.derive_turn(session_id))
            .map(turn_state_label)
            .map(str::to_string),
        latest_request_id: snapshot.latest_request_id_for_session(session_id),
        conversation_updated_at: snapshot
            .conversations
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| row.updated_at.clone()),
        request: request.map(|row| RequestRowDiagnostics {
            status: row.status.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            failure_reason: row.failure_reason.clone(),
            created_at: row.created_at.clone(),
            claimed_at: row.claimed_at.clone(),
            interrupt_requested_at: row.interrupt_requested_at.clone(),
            valid_until: row.valid_until.clone(),
        }),
        response: response.map(|row| ResponseRowDiagnostics {
            status: row.status.clone(),
            error_message: row.error_message.clone(),
            progress_seq: row.progress_seq,
            materialized_message_sequence: row.materialized_message_sequence,
            materialized_at: row.materialized_at.clone(),
            completed_at: row.completed_at.clone(),
            content_len: row.content.as_deref().map_or(0, str::len),
            reasoning_len: row.reasoning.as_deref().map_or(0, str::len),
        }),
        matching_response_count: matching_responses.len(),
        matching_response_progress_seqs: matching_responses
            .iter()
            .map(|row| row.progress_seq.unwrap_or_default())
            .collect(),
        matching_response_statuses: matching_responses
            .iter()
            .map(|row| row.status.clone().unwrap_or_default())
            .collect(),
        tool_calls: ToolCallDiagnostics {
            total: relevant_tool_calls.len(),
            completed: completed_tool_calls,
            pending: relevant_tool_calls
                .len()
                .saturating_sub(completed_tool_calls),
            latest_tool_name: latest_tool_call.and_then(|row| row.tool_name.clone()),
            latest_status: latest_tool_call.and_then(|row| row.status.clone()),
            latest_completed_at: latest_tool_call.and_then(|row| row.completed_at.clone()),
        },
        tool_result_count: transcript.tool_results.len(),
        message_count: transcript.messages.len(),
        timeline_count: session_snapshot
            .as_ref()
            .map_or(0, |session| session.timeline_items.len()),
        active_response_overlay_content_len: session_snapshot
            .as_ref()
            .and_then(|session| session.active_response_overlay.as_ref())
            .and_then(|overlay| overlay.content.as_deref())
            .map_or(0, str::len),
        active_response_overlay_reasoning_len: session_snapshot
            .as_ref()
            .and_then(|session| session.active_response_overlay.as_ref())
            .and_then(|overlay| overlay.reasoning.as_deref())
            .map_or(0, str::len),
    }
}

async fn refresh_store_with_timeout(core: &ClientCore) -> Option<String> {
    match tokio::time::timeout(Duration::from_secs(5), core.refresh_store()).await {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("timed out refreshing store".to_string()),
    }
}
