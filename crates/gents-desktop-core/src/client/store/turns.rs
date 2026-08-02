use std::collections::HashSet;

use gents_protocol::client_protocol::{
    derive_turn as derive_client_turn, AttemptView, RequestLifecycleState, RequestSnapshot,
    ResponseSnapshot, ResponseStatus,
};
use gents_protocol::row::AgentResponseRow;

use super::indexing::clean_string;
use super::ClientStore;

pub(super) fn derive_turn(
    store: &ClientStore,
    session_id: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let latest_request_id = store.latest_request_id_for_session(session_id)?;
    let attempts = attempt_chain_for_request(store, &latest_request_id);
    derive_client_turn(&attempts)
}

pub(super) fn derive_turn_for_agent(
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let latest_request_id = store.latest_request_id_for_session_for_agent(session_id, agent_did)?;
    let attempts = attempt_chain_for_request_for_agent(store, &latest_request_id, agent_did);
    derive_client_turn(&attempts)
}

pub(super) fn derive_turn_for_request(
    store: &ClientStore,
    request_id: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let attempts = attempt_chain_for_request(store, request_id);
    derive_client_turn(&attempts)
}

pub(super) fn derive_turn_for_request_for_agent(
    store: &ClientStore,
    request_id: &str,
    agent_did: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let attempts = attempt_chain_for_request_for_agent(store, request_id, agent_did);
    derive_client_turn(&attempts)
}

fn attempt_chain_for_request(store: &ClientStore, request_id: &str) -> Vec<AttemptView> {
    let mut attempts = Vec::new();
    let mut cursor = Some(request_id.to_string());
    let mut seen = HashSet::new();

    while let Some(current_request_id) = cursor.take() {
        if !seen.insert(current_request_id.clone()) {
            break;
        }
        let Some(index) = store.request_index_by_id.get(&current_request_id).copied() else {
            break;
        };
        let row = &store.requests[index];
        if let Some(attempt) = attempt_for_request(store, index) {
            attempts.push(attempt);
        }
        cursor = clean_string(row.retry_parent_request.as_deref());
    }

    attempts
}

fn attempt_chain_for_request_for_agent(
    store: &ClientStore,
    request_id: &str,
    agent_did: &str,
) -> Vec<AttemptView> {
    let mut attempts = Vec::new();
    let mut cursor = Some(request_id.to_string());
    let mut seen = HashSet::new();

    while let Some(current_request_id) = cursor.take() {
        if !seen.insert(current_request_id.clone()) {
            break;
        }
        let Some((index, row)) = store.requests.iter().enumerate().find(|(_index, row)| {
            row.request_id == current_request_id
                && row.agent_did.as_deref().is_none_or(|did| did == agent_did)
        }) else {
            break;
        };
        if let Some(attempt) = attempt_for_request_for_agent(store, index, agent_did) {
            attempts.push(attempt);
        }
        cursor = clean_string(row.retry_parent_request.as_deref());
    }

    attempts
}

fn attempt_for_request(store: &ClientStore, index: usize) -> Option<AttemptView> {
    let row = &store.requests[index];
    let lifecycle =
        RequestLifecycleState::try_from(row.lifecycle_state.as_deref().unwrap_or_default()).ok()?;

    let response = store
        .latest_response_by_request_id
        .get(&row.request_id)
        .and_then(|response_index| response_status(&store.responses[*response_index]))
        .map(|status| ResponseSnapshot { status });

    Some(AttemptView {
        request: RequestSnapshot {
            request_id: row.request_id.clone(),
            retry_parent_request: clean_string(row.retry_parent_request.as_deref()),
            lifecycle_state: lifecycle,
            is_superseded: clean_string(row.superseded_by_request.as_deref()).is_some(),
        },
        response,
    })
}

fn attempt_for_request_for_agent(
    store: &ClientStore,
    index: usize,
    agent_did: &str,
) -> Option<AttemptView> {
    let row = &store.requests[index];
    let lifecycle =
        RequestLifecycleState::try_from(row.lifecycle_state.as_deref().unwrap_or_default()).ok()?;
    let response = store
        .latest_response_for_request_for_agent(&row.request_id, agent_did)
        .and_then(response_status)
        .map(|status| ResponseSnapshot { status });

    Some(AttemptView {
        request: RequestSnapshot {
            request_id: row.request_id.clone(),
            retry_parent_request: clean_string(row.retry_parent_request.as_deref()),
            lifecycle_state: lifecycle,
            is_superseded: clean_string(row.superseded_by_request.as_deref()).is_some(),
        },
        response,
    })
}

fn response_status(row: &AgentResponseRow) -> Option<ResponseStatus> {
    match row.status.as_deref().unwrap_or_default() {
        "streaming" => Some(ResponseStatus::Streaming),
        "complete" | "completed" => Some(ResponseStatus::Complete),
        "error" | "failed" => Some(ResponseStatus::Error),
        _ => None,
    }
}
