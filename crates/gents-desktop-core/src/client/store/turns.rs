use std::collections::HashSet;

use gents_protocol::client_protocol::{
    derive_turn as derive_client_turn, AttemptView, RequestSnapshot,
};

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use super::indexing::clean_string;
use super::ClientStore;

pub(super) fn derive_turn(
    store: &ClientStore,
    session_id: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let turn_request_id = store.turn_request_id_for_session(session_id)?;
    let attempts = attempt_chain_for_request(store, &turn_request_id);
    derive_client_turn(&attempts)
}

pub(super) fn derive_turn_for_agent(
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let turn_request_id = store.turn_request_id_for_session_for_agent(session_id, agent_did)?;
    let attempts = attempt_chain_for_request_for_agent(store, &turn_request_id, agent_did);
    derive_client_turn(&attempts)
}

fn unclaimed(row: &AgentRequestRow) -> bool {
    matches!(
        row.lifecycle_state,
        Some(RequestLifecycleState::Pending | RequestLifecycleState::WorkspaceBindingPending)
    )
}

/// The request whose turn a session is on, reached from `newest` within the
/// session's `requests`. A message folded into a claimed request is answered
/// by that request (`gents::lifecycle::folded_into`, Lean
/// `SessionQueue.claimFolding`), and an unclaimed request queued behind a
/// non-terminal request waits behind it (Lean
/// `SessionObservation.queuedRequests`), so neither is the session's turn.
pub fn session_turn_request<'a>(
    requests: &[&'a AgentRequestRow],
    newest: &'a AgentRequestRow,
) -> &'a AgentRequestRow {
    let mut current = newest;
    let mut seen = HashSet::new();
    while seen.insert(current.request_id.as_str()) {
        let next = if let Some(head) = gents::lifecycle::folded_into(current) {
            let head_doc = clean_string(current.superseded_by_request_doc_id.as_deref());
            requests.iter().copied().find(|row| {
                row.request_id == head
                    && head_doc
                        .as_deref()
                        .is_none_or(|doc| row.doc_id.as_deref() == Some(doc))
            })
        } else if unclaimed(current) {
            current
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| clean_string(queue.queued_after_request_id.as_deref()))
                .and_then(|ahead| {
                    requests.iter().copied().find(|row| {
                        row.request_id == ahead
                            && row
                                .lifecycle_state
                                .is_some_and(|state| !state.is_terminal())
                    })
                })
        } else {
            None
        };
        match next {
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

/// Unclaimed requests waiting behind `turn`, in queue order.
pub fn queued_behind_turn<'a>(
    requests: &[&'a AgentRequestRow],
    turn: &AgentRequestRow,
) -> Vec<&'a AgentRequestRow> {
    let mut queued = requests
        .iter()
        .copied()
        .filter(|row| {
            row.request_id != turn.request_id
                && unclaimed(row)
                && session_turn_request(requests, row).request_id == turn.request_id
        })
        .collect::<Vec<_>>();
    queued.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    queued
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
        if let Some(attempt) = attempt_for_request(store, index) {
            attempts.push(attempt);
        }
        cursor = clean_string(store.requests[index].retry_parent_request.as_deref());
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
        if let Some(attempt) = attempt_for_request(store, index) {
            attempts.push(attempt);
        }
        cursor = clean_string(row.retry_parent_request.as_deref());
    }

    attempts
}

fn attempt_for_request(store: &ClientStore, index: usize) -> Option<AttemptView> {
    let row = &store.requests[index];
    let lifecycle = row.lifecycle_state?;

    Some(AttemptView {
        request: RequestSnapshot {
            request_id: row.request_id.clone(),
            retry_parent_request: clean_string(row.retry_parent_request.as_deref()),
            lifecycle_state: lifecycle,
            is_superseded: clean_string(row.superseded_by_request.as_deref()).is_some(),
        },
    })
}
