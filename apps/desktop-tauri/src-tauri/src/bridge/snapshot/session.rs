use defra_agent_protocol::transcript::{normalize_markdown_text, present_persisted_message};

use super::super::cause_derivation::{
    derive_response_cause, derive_tool_call_cause, RequestEvidence, ResponseEvidence,
    ToolCallEvidence,
};
use super::super::types::{
    normalize_optional, turn_state_label, DesktopSessionSnapshot, MessageView, PendingTurnView,
    ResponseView, ToolCallView, ToolResultView,
};
use super::timeline::{build_rendered_timeline, materialized_user_turn_count};
use super::{request_matches_agent, source_matches_agent};

#[cfg(test)]
pub(crate) fn build_session_snapshot_from_store(
    store: &defra_agent_desktop_core::client::ClientStore,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    build_session_snapshot_from_store_for_agent(store, None, session_id, preferred_request_id)
}

pub(crate) fn build_session_snapshot_from_store_for_agent(
    store: &defra_agent_desktop_core::client::ClientStore,
    agent_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let conversation = store.conversations.iter().find(|row| {
        row.session_id == session_id
            && agent_did.map_or(true, |agent_did| {
                row.agent_did.as_deref() == Some(agent_did)
            })
    });
    let session_row = store
        .sessions
        .iter()
        .enumerate()
        .find(|(index, row)| {
            row.session_id == session_id
                && agent_did.map_or(true, |agent_did| {
                    source_matches_agent(&store.session_source_agent_dids, *index, agent_did, false)
                })
        })
        .map(|(_index, row)| row);
    let requests = agent_did.map_or_else(
        || store.requests_for_session(session_id),
        |agent_did| store.requests_for_session_for_agent(session_id, agent_did),
    );

    if conversation.is_none() && session_row.is_none() && requests.is_empty() {
        return None;
    }

    let transcript = agent_did.map_or_else(
        || store.transcript(session_id),
        |agent_did| store.transcript_for_agent(session_id, agent_did),
    );
    let latest_request_id = preferred_request_id
        .filter(|request_id| requests.iter().any(|row| row.request_id == *request_id))
        .map(str::to_owned)
        .or_else(|| {
            agent_did.map_or_else(
                || store.latest_request_id_for_session(session_id),
                |agent_did| store.latest_request_id_for_session_for_agent(session_id, agent_did),
            )
        });
    let latest_request = latest_request_id
        .as_deref()
        .and_then(|request_id| {
            requests
                .iter()
                .find(|row| row.request_id == request_id)
                .copied()
        })
        .or_else(|| requests.last().copied());
    let turn_state = latest_request_id
        .as_deref()
        .and_then(|request_id| store.derive_turn_for_request(request_id))
        .or_else(|| {
            if agent_did.is_none() {
                store.derive_turn(session_id)
            } else {
                None
            }
        });
    let turn_state_label = turn_state.map(turn_state_label).map(str::to_owned);
    let latest_response = latest_request_id
        .as_deref()
        .and_then(|request_id| {
            agent_did.map_or_else(
                || store.latest_response_for_request(request_id),
                |agent_did| store.latest_response_for_request_for_agent(request_id, agent_did),
            )
        })
        .map(|row| {
            let req_evidence = latest_request
                .map(|r| RequestEvidence {
                    request_id: r.request_id.clone(),
                    interrupt_requested_at: r.interrupt_requested_at.clone(),
                    caused_by_parent_request_id: r.retry_parent_request.clone(),
                    deadline_breached: false,
                })
                .unwrap_or_default();
            let resp_evidence = ResponseEvidence {
                interrupted_at: normalize_optional(row.interrupted_at.as_deref()),
                completed_at: normalize_optional(row.completed_at.as_deref()),
            };
            let cancel_cause = derive_response_cause(&req_evidence, &resp_evidence);
            ResponseView {
                status: normalize_optional(row.status.as_deref()),
                content: row
                    .content
                    .as_deref()
                    .map(normalize_markdown_text)
                    .filter(|value| !value.is_empty()),
                reasoning: row
                    .reasoning
                    .as_deref()
                    .map(normalize_markdown_text)
                    .filter(|value| !value.is_empty()),
                error_message: normalize_optional(row.error_message.as_deref()),
                token_count: row.token_count,
                materialized_message_sequence: row.materialized_message_sequence,
                materialized_at: normalize_optional(row.materialized_at.as_deref()),
                interrupted_at: normalize_optional(row.interrupted_at.as_deref()),
                completed_at: normalize_optional(row.completed_at.as_deref()),
                cancel_cause,
            }
        });
    let active_response_overlay = latest_response.clone().filter(|response| {
        let response_status = response
            .status
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        matches!(
            turn_state,
            Some(defra_agent_protocol::client_protocol::ClientTurnState::WaitingForClaim)
                | Some(defra_agent_protocol::client_protocol::ClientTurnState::Streaming)
        ) && response.materialized_message_sequence.is_none()
            && response.interrupted_at.is_none()
            && !matches!(
                response_status.as_str(),
                "complete" | "completed" | "error" | "failed"
            )
            && (response
                .content
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || response
                    .reasoning
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    });
    let pending_turn = latest_request_id
        .as_deref()
        .and_then(|request_id| build_pending_turn(store, agent_did, session_id, request_id));

    let messages = transcript
        .messages
        .into_iter()
        .map(|row| {
            let role = normalize_optional(row.role.as_deref());
            let content = normalize_optional(row.content.as_deref());
            let presentation = role
                .as_deref()
                .zip(content.as_deref())
                .map(|(role, content)| present_persisted_message(role, content));

            MessageView {
                message_key: row.message_key.clone(),
                sequence: row.sequence,
                role,
                content,
                display_role: presentation
                    .as_ref()
                    .map(|presentation| presentation.role.label().to_ascii_lowercase()),
                display_content: presentation.as_ref().and_then(|presentation| {
                    normalize_optional(Some(presentation.body_markdown.as_str()))
                }),
                reasoning: presentation.as_ref().and_then(|presentation| {
                    presentation
                        .reasoning_markdown
                        .as_deref()
                        .and_then(|reasoning| normalize_optional(Some(reasoning)))
                }),
                has_tool_calls: presentation
                    .as_ref()
                    .is_some_and(|presentation| presentation.has_tool_calls),
                has_tool_results: presentation
                    .as_ref()
                    .is_some_and(|presentation| presentation.has_tool_results),
                timestamp: normalize_optional(row.timestamp.as_deref()),
            }
        })
        .collect::<Vec<_>>();

    let tool_call_req_evidence = latest_request
        .map(|r| RequestEvidence {
            request_id: r.request_id.clone(),
            interrupt_requested_at: r.interrupt_requested_at.clone(),
            caused_by_parent_request_id: r.retry_parent_request.clone(),
            deadline_breached: false,
        })
        .unwrap_or_default();
    let tool_calls = transcript
        .tool_calls
        .into_iter()
        .map(|row| {
            let tool_evidence = ToolCallEvidence {
                tool_call_id: row.tool_call_id.clone().unwrap_or_default(),
                lifecycle_state: row.lifecycle_state.clone(),
                deadline_at: row.deadline_at.clone(),
                // TODO(#277-followup): cancel_policy not on AgentToolCallRow yet — see follow-up
                // for promoting that field through the protocol crate so tool-call
                // interrupted-via-cascade can be derived from snapshot data.
                cancel_policy: None,
                completed_at: row.completed_at.clone(),
                timed_out: row.lifecycle_state.as_deref() == Some("timedOut"),
            };
            let cancel_cause = derive_tool_call_cause(&tool_call_req_evidence, &tool_evidence);
            ToolCallView {
                tool_call_key: row.tool_call_key.clone(),
                message_sequence: row.message_sequence,
                tool_name: normalize_optional(row.tool_name.as_deref()),
                tool_call_id: normalize_optional(row.tool_call_id.as_deref()),
                args: normalize_optional(row.args.as_deref()),
                result: normalize_optional(row.result.as_deref()),
                status: normalize_optional(row.status.as_deref()),
                started_at: normalize_optional(row.started_at.as_deref()),
                completed_at: normalize_optional(row.completed_at.as_deref()),
                cancel_cause,
            }
        })
        .collect::<Vec<_>>();

    let tool_results = transcript
        .tool_results
        .into_iter()
        .map(|row| ToolResultView {
            tool_name: normalize_optional(row.tool_name.as_deref()),
            tool_input: normalize_optional(row.tool_input.as_deref()),
            output_text: normalize_optional(row.output_text.as_deref()),
            truncated: row.truncated,
            created_at: normalize_optional(row.created_at.as_deref()),
        })
        .collect::<Vec<_>>();

    let timeline_items = build_rendered_timeline(
        &messages,
        &tool_calls,
        pending_turn.as_ref(),
        active_response_overlay.as_ref(),
    );

    Some(DesktopSessionSnapshot {
        session_id: session_id.to_string(),
        agent_did: conversation
            .and_then(|row| normalize_optional(row.agent_did.as_deref()))
            .or_else(|| {
                latest_request.and_then(|row| normalize_optional(row.agent_did.as_deref()))
            }),
        behavior_id: conversation
            .and_then(|row| normalize_optional(row.behavior_id.as_deref()))
            .or_else(|| session_row.and_then(|row| normalize_optional(row.behavior_id.as_deref())))
            .or_else(|| {
                latest_request.and_then(|row| normalize_optional(row.behavior_id.as_deref()))
            }),
        title: conversation.and_then(|row| normalize_optional(row.title.as_deref())),
        preview_text: conversation.and_then(|row| normalize_optional(row.preview_text.as_deref())),
        status: conversation
            .and_then(|row| normalize_optional(row.status.as_deref()))
            .or_else(|| session_row.and_then(|row| normalize_optional(row.status.as_deref()))),
        turn_state: turn_state_label,
        latest_request_id,
        latest_response,
        active_response_overlay,
        pending_turn,
        timeline_items,
        messages,
        tool_calls,
        tool_results,
    })
}

fn request_turn_root_id(request: &defra_agent_protocol::row::AgentRequestRow) -> String {
    normalize_optional(request.retry_root_request.as_deref())
        .unwrap_or_else(|| request.request_id.clone())
}

fn logical_turn_roots_for_session(
    store: &defra_agent_desktop_core::client::ClientStore,
    agent_did: Option<&str>,
    session_id: &str,
) -> Vec<String> {
    let mut requests = agent_did.map_or_else(
        || store.requests_for_session(session_id),
        |agent_did| store.requests_for_session_for_agent(session_id, agent_did),
    );
    requests.sort_by(|left, right| {
        normalize_optional(left.created_at.as_deref())
            .cmp(&normalize_optional(right.created_at.as_deref()))
            .then_with(|| left.request_id.cmp(&right.request_id))
    });

    let mut roots = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for request in requests {
        let root_id = request_turn_root_id(request);
        if seen.insert(root_id.clone()) {
            roots.push(root_id);
        }
    }

    roots
}

fn logical_turn_index_for_request(
    store: &defra_agent_desktop_core::client::ClientStore,
    agent_did: Option<&str>,
    session_id: &str,
    request_id: &str,
) -> Option<usize> {
    let request = store.requests.iter().find(|row| {
        row.request_id == request_id
            && row.session_id.as_deref() == Some(session_id)
            && agent_did.map_or(true, |agent_did| {
                request_matches_agent(row, agent_did, false)
            })
    })?;
    let root_id = request_turn_root_id(request);
    logical_turn_roots_for_session(store, agent_did, session_id)
        .iter()
        .position(|candidate| candidate == &root_id)
}

fn build_pending_turn(
    store: &defra_agent_desktop_core::client::ClientStore,
    agent_did: Option<&str>,
    session_id: &str,
    request_id: &str,
) -> Option<PendingTurnView> {
    let request = store.requests.iter().find(|row| {
        row.request_id == request_id
            && row.session_id.as_deref() == Some(session_id)
            && agent_did.map_or(true, |agent_did| {
                request_matches_agent(row, agent_did, false)
            })
    })?;

    let lifecycle_state = normalize_optional(request.lifecycle_state.as_deref());
    let content = normalize_optional(request.content.as_deref())?;
    let active_turn_index =
        logical_turn_index_for_request(store, agent_did, session_id, request_id)?;
    let transcript = agent_did.map_or_else(
        || store.transcript(session_id),
        |agent_did| store.transcript_for_agent(session_id, agent_did),
    );
    let messages = transcript
        .messages
        .into_iter()
        .map(|row| {
            let role = normalize_optional(row.role.as_deref());
            let body = normalize_optional(row.content.as_deref());
            let presentation = role
                .as_deref()
                .zip(body.as_deref())
                .map(|(role, content)| present_persisted_message(role, content));

            MessageView {
                message_key: row.message_key.clone(),
                sequence: row.sequence,
                role,
                content: body,
                display_role: presentation
                    .as_ref()
                    .map(|presentation| presentation.role.label().to_ascii_lowercase()),
                display_content: presentation.as_ref().and_then(|presentation| {
                    normalize_optional(Some(presentation.body_markdown.as_str()))
                }),
                reasoning: None,
                has_tool_calls: false,
                has_tool_results: false,
                timestamp: normalize_optional(row.timestamp.as_deref()),
            }
        })
        .collect::<Vec<_>>();

    if materialized_user_turn_count(&messages) > active_turn_index {
        return None;
    }

    Some(PendingTurnView {
        request_id: request.request_id.clone(),
        content: content.to_string(),
        lifecycle_state,
        created_at: normalize_optional(request.created_at.as_deref()),
    })
}
