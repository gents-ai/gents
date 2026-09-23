use std::time::Duration;

use gents_desktop_core::client::ClientCore;
use serde::Serialize;

use crate::live_fixture::LiveBridgeFixture;
use gents_desktop_bridge::snapshot::{
    apply_session_timeline_page_with_query, build_runtime_snapshot,
    build_session_snapshot_for_agent_with_transcript,
};
use gents_desktop_bridge::types::{
    turn_state_label, DesktopClientSnapshot, DesktopSessionSnapshot, RenderedTimelineItem,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestRowDiagnostics {
    doc_id: Option<String>,
    lifecycle_state: Option<String>,
    failure_reason: Option<String>,
    created_at: Option<String>,
    claimed_at: Option<String>,
    interrupt_requested_at: Option<String>,
    valid_until: Option<String>,
    terminal_output: Option<serde_json::Value>,
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
    transcript_diagnostics_error: Option<String>,
    turn_state: Option<String>,
    latest_request_id: Option<String>,
    session_updated_at: Option<String>,
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
    raw_message_count_for_request: usize,
    raw_segment_count_for_request: usize,
    raw_message_doc_ids: Vec<String>,
    raw_segment_doc_ids: Vec<String>,
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
    if let Err(error) = overlay_fixture_operator_config(fixture).await {
        tracing::warn!(
            agent_did = fixture.agent_did(),
            error = %error,
            "live bridge runtime-owned config overlay failed"
        );
    }
    DesktopClientSnapshot {
        bootstrap: fixture.build_bootstrap_summary().await,
        client: Some(build_runtime_snapshot(fixture.desktop_core().as_ref()).await),
    }
}

/// The live fixture owns the real runtime node, but deliberately advertises a
/// dummy operator GraphQL URL because its P2P topology is managed in-process.
/// Load that node directly through the normal query owner, then use the same
/// operator overlay as production so runtime observations gain their truthful
/// agent source instead of being mistaken for unscoped replica rows.
async fn overlay_fixture_operator_config(fixture: &LiveBridgeFixture) -> anyhow::Result<()> {
    if let Some(error) = refresh_store_with_timeout(fixture.remote_core().as_ref()).await {
        anyhow::bail!(error);
    }
    let remote = fixture.remote_core().store().snapshot();
    let local = fixture.desktop_core().store().snapshot();
    let overlayed = local.overlay_agent_operator_config(fixture.agent_did(), &remote);
    fixture.desktop_core().store().replace_snapshot(overlayed);
    Ok(())
}

pub(crate) async fn build_desktop_session_snapshot(
    fixture: &LiveBridgeFixture,
    agent_did: Option<&str>,
    session_id: &str,
    request_id: Option<&str>,
    timeline_limit: Option<usize>,
    timeline_before_item_key: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let _ = refresh_store_with_timeout(fixture.desktop_core().as_ref()).await;
    let resolved_agent_did = agent_did.map(str::to_owned).or_else(|| {
        fixture
            .desktop_core()
            .store()
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .map(|session| session.agent_did.clone())
    });
    let agent_did = resolved_agent_did.as_deref();
    // A local-standard route is self-authored by the target agent. Production
    // enrollment uses the desktop principal instead; this fixture deliberately
    // bypasses enrollment, so its transcript namespace is the agent DID rather
    // than requester_did:null or the desktop principal.
    let requester_scope = fixture.requester_scope(agent_did, session_id, request_id);
    let page = match gents_desktop_core::client::load_session_transcript_page(
        fixture.remote_core().node(),
        session_id,
        agent_did,
        requester_scope.as_deref(),
        timeline_before_item_key,
        timeline_limit,
    )
    .await
    {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(
                session_id,
                error = %error,
                "live bridge bounded transcript query failed"
            );
            return None;
        }
    };
    let context_store = if timeline_before_item_key.is_none() {
        match gents_desktop_core::client::load_session_context_store(
            fixture.remote_core().node(),
            session_id,
            agent_did,
            requester_scope.as_deref(),
        )
        .await
        {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::warn!(
                    session_id,
                    error = %error,
                    "live bridge session context query failed; using bounded inexact totals"
                );
                None
            }
        }
    } else {
        None
    };
    let mut snapshot = build_session_snapshot_for_agent_with_transcript(
        fixture.desktop_core().as_ref(),
        agent_did,
        session_id,
        request_id,
        Some(&page.store),
        Some(&page.canonical_dependencies),
        context_store.as_ref(),
        context_store.is_some(),
        timeline_before_item_key.is_none(),
    )
    .await?;
    if let Err(error) = apply_session_timeline_page_with_query(
        &mut snapshot,
        timeline_before_item_key,
        timeline_limit,
        Some(&page),
    ) {
        tracing::warn!(
            session_id,
            error,
            "live bridge transcript page projection failed"
        );
        return None;
    }
    Some(snapshot)
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
            fixture.requester_scope(Some(fixture.agent_did()), session_id, Some(request_id)),
        )
        .await,
        remote: build_request_diagnostics(
            "remote",
            fixture.remote_core().as_ref(),
            session_id,
            request_id,
            fixture.requester_scope(Some(fixture.agent_did()), session_id, Some(request_id)),
        )
        .await,
    }
}

async fn build_request_diagnostics(
    source: &str,
    core: &ClientCore,
    session_id: &str,
    request_id: &str,
    requester_scope: Option<String>,
) -> RequestDiagnostics {
    let refresh_error = refresh_store_with_timeout(core).await;
    let snapshot = core.store().snapshot();
    let request = snapshot
        .requests
        .iter()
        .find(|row| row.request_id == request_id)
        .cloned();
    let resolved_agent_did = snapshot
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .map(|session| session.agent_did.clone());
    // The rendered session uses the same bounded page as the app. Counts use a
    // separate explicit diagnostic read so acceptance evidence is exact rather
    // than silently capped by the interactive row budget.
    let transcript_page = gents_desktop_core::client::load_session_transcript_page(
        core.node(),
        session_id,
        resolved_agent_did.as_deref(),
        requester_scope.as_deref(),
        None,
        Some(gents_desktop_core::client::MAX_SESSION_TRANSCRIPT_PAGE_SIZE),
    )
    .await
    .ok();
    let transcript_store = transcript_page.as_ref().map(|page| &page.store);
    let (diagnostics_store, transcript_diagnostics_error) =
        match gents_desktop_core::client::load_session_diagnostics_store(
            core.node(),
            session_id,
            resolved_agent_did.as_deref(),
            requester_scope.as_deref(),
        )
        .await
        {
            Ok(store) => (Some(store), None),
            Err(error) => (None, Some(error.to_string())),
        };
    let transcript = diagnostics_store
        .as_ref()
        .unwrap_or(snapshot.as_ref())
        .transcript(session_id);
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
    let session_snapshot = build_session_snapshot_for_agent_with_transcript(
        core,
        resolved_agent_did.as_deref(),
        session_id,
        Some(request_id),
        transcript_store,
        None,
        None,
        false,
        true,
    )
    .await;
    let matching_responses = session_snapshot
        .as_ref()
        .into_iter()
        .flat_map(|session| session.messages.iter())
        .filter(|row| {
            row.request_id.as_deref() == Some(request_id)
                && row.display_role.as_deref() == Some("assistant")
        })
        .collect::<Vec<_>>();
    let response = matching_responses.last().map(|row| ResponseRowDiagnostics {
        status: Some("complete".to_string()),
        error_message: row.reconstruction_error.clone(),
        progress_seq: row.sequence,
        materialized_message_sequence: row.sequence,
        materialized_at: row.timestamp.clone(),
        completed_at: row.timestamp.clone(),
        content_len: row.display_content.as_deref().map_or(0, str::len),
        reasoning_len: row.reasoning.as_deref().map_or(0, str::len),
    });
    let (live_content_len, live_reasoning_len) = session_snapshot
        .as_ref()
        .and_then(|session| {
            session
                .timeline_items
                .iter()
                .rev()
                .find_map(|item| match item {
                    RenderedTimelineItem::LiveAssistant {
                        content, reasoning, ..
                    } => Some((
                        content.as_deref().map_or(0, str::len),
                        reasoning.as_deref().map_or(0, str::len),
                    )),
                    _ => None,
                })
        })
        .unwrap_or_default();
    let request_doc_id = request.as_ref().and_then(|row| row.doc_id.clone());
    let canonical_store = diagnostics_store.as_ref().unwrap_or(snapshot.as_ref());
    let raw_messages = canonical_store
        .transcript_messages
        .iter()
        .filter(|row| row.message.request_doc_id.as_deref() == request_doc_id.as_deref())
        .collect::<Vec<_>>();
    let raw_segments = canonical_store
        .output_segments
        .iter()
        .filter(|row| Some(row.segment.request_doc_id.as_str()) == request_doc_id.as_deref())
        .collect::<Vec<_>>();

    RequestDiagnostics {
        source: source.to_string(),
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        refresh_error,
        transcript_diagnostics_error,
        turn_state: snapshot
            .derive_turn_for_request(request_id)
            .or_else(|| snapshot.derive_turn(session_id))
            .map(turn_state_label)
            .map(str::to_string),
        latest_request_id: snapshot.latest_request_id_for_session(session_id),
        session_updated_at: snapshot
            .sessions
            .iter()
            .find(|row| row.session_id == session_id)
            .and_then(|row| {
                row.observation
                    .as_ref()
                    .map(|observation| observation.last_activity_at.clone())
            }),
        request: request.map(|row| RequestRowDiagnostics {
            doc_id: row.doc_id.clone(),
            lifecycle_state: row.lifecycle_state.map(|state| state.as_str().to_string()),
            failure_reason: row.failure_reason.clone(),
            created_at: row.created_at.clone(),
            claimed_at: row.claimed_at.clone(),
            interrupt_requested_at: row.interrupt_requested_at.clone(),
            valid_until: row.valid_until.clone(),
            terminal_output: row
                .terminal_output
                .as_ref()
                .and_then(|output| serde_json::to_value(output).ok()),
        }),
        response,
        matching_response_count: matching_responses.len(),
        matching_response_progress_seqs: matching_responses
            .iter()
            .map(|row| row.sequence.unwrap_or_default())
            .collect(),
        matching_response_statuses: matching_responses
            .iter()
            .map(|_| "complete".to_string())
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
        tool_result_count: session_snapshot
            .as_ref()
            .into_iter()
            .flat_map(|session| session.messages.iter())
            .filter(|row| row.has_tool_results)
            .count(),
        message_count: transcript.messages.len(),
        timeline_count: session_snapshot
            .as_ref()
            .map_or(0, |session| session.timeline_items.len()),
        active_response_overlay_content_len: live_content_len,
        active_response_overlay_reasoning_len: live_reasoning_len,
        raw_message_count_for_request: raw_messages.len(),
        raw_segment_count_for_request: raw_segments.len(),
        raw_message_doc_ids: raw_messages.iter().map(|row| row.doc_id.clone()).collect(),
        raw_segment_doc_ids: raw_segments.iter().map(|row| row.doc_id.clone()).collect(),
    }
}

async fn refresh_store_with_timeout(core: &ClientCore) -> Option<String> {
    match tokio::time::timeout(Duration::from_secs(5), core.refresh_store()).await {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("timed out refreshing store".to_string()),
    }
}
