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

pub(super) fn derive_turn_for_node(
    store: &ClientStore,
    session_id: &str,
    node_did: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let turn_request_id = store.turn_request_id_for_session_for_node(session_id, node_did)?;
    let attempts = attempt_chain_for_request_for_node(store, &turn_request_id, node_did);
    derive_client_turn(&attempts)
}

fn unclaimed(row: &AgentRequestRow) -> bool {
    matches!(
        row.lifecycle_state,
        Some(RequestLifecycleState::Pending | RequestLifecycleState::WorkspaceBindingPending)
    )
}

/// The request whose turn a session is on, reached from `newest` within the
/// session's `requests` (Lean `ClientShell.SessionTurn.turnOf`). Every step
/// stays in `newest`'s requester scope.
pub fn session_turn_request<'a>(
    requests: &[&'a AgentRequestRow],
    newest: &'a AgentRequestRow,
) -> &'a AgentRequestRow {
    resolve(requests, requests.len() + 1, newest)
}

fn in_scope<'a>(
    requests: &[&'a AgentRequestRow],
    of: &AgentRequestRow,
    matches: impl Fn(&AgentRequestRow) -> bool,
) -> Option<&'a AgentRequestRow> {
    requests
        .iter()
        .copied()
        .find(|row| row.requester_did == of.requester_did && matches(row))
}

/// Lean `SessionTurn.resolve`: a folded row resolves to the physical request
/// it was folded into, a terminal row to its retry successor, and an
/// unclaimed queued row to the turn of the request it was queued after while
/// that turn is not terminal.
fn resolve<'a>(
    requests: &[&'a AgentRequestRow],
    fuel: usize,
    row: &'a AgentRequestRow,
) -> &'a AgentRequestRow {
    if fuel == 0 {
        return row;
    }
    if gents::lifecycle::folded_into(row).is_some() {
        let owner = clean_string(row.superseded_by_request_doc_id.as_deref()).and_then(|doc| {
            in_scope(requests, row, |candidate| {
                candidate.doc_id.as_deref() == Some(doc.as_str())
            })
        });
        return owner.map_or(row, |owner| resolve(requests, fuel - 1, owner));
    }
    match row.lifecycle_state {
        Some(state) if state.is_terminal() => {
            let next = in_scope(requests, row, |candidate| {
                clean_string(candidate.retry_parent_request.as_deref()).as_deref()
                    == Some(row.request_id.as_str())
            });
            next.map_or(row, |next| resolve(requests, fuel - 1, next))
        }
        _ if unclaimed(row) => {
            let ahead = row
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| clean_string(queue.queued_after_request_id.as_deref()))
                .and_then(|ahead| {
                    in_scope(requests, row, |candidate| candidate.request_id == ahead)
                });
            match ahead.map(|ahead| resolve(requests, fuel - 1, ahead)) {
                Some(turn)
                    if turn
                        .lifecycle_state
                        .is_some_and(|state| !state.is_terminal()) =>
                {
                    turn
                }
                _ => row,
            }
        }
        _ => row,
    }
}

/// Unclaimed requests waiting behind `turn`, in arrival order (Lean
/// `SessionTurn.queuedBehind`).
pub fn queued_behind_turn<'a>(
    requests: &[&'a AgentRequestRow],
    turn: &AgentRequestRow,
) -> Vec<&'a AgentRequestRow> {
    let mut queued = requests
        .iter()
        .copied()
        .filter(|row| {
            row.doc_id != turn.doc_id
                && unclaimed(row)
                && session_turn_request(requests, row).doc_id == turn.doc_id
        })
        .collect::<Vec<_>>();
    queued.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    queued
}

/// Requests a claim folded, in `requester`'s scope (Lean `SessionTurn.foldedIn`).
pub fn folded_requests<'a>(
    requests: &[&'a AgentRequestRow],
    requester: Option<&str>,
) -> Vec<&'a AgentRequestRow> {
    requests
        .iter()
        .copied()
        .filter(|row| {
            row.requester_did.as_deref() == requester
                && gents::lifecycle::folded_into(row).is_some()
        })
        .collect()
}

pub(super) fn derive_turn_for_request(
    store: &ClientStore,
    request_id: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let attempts = attempt_chain_for_request(store, request_id);
    derive_client_turn(&attempts)
}

pub(super) fn derive_turn_for_request_for_node(
    store: &ClientStore,
    request_id: &str,
    node_did: &str,
) -> Option<gents_protocol::client_protocol::ClientTurnState> {
    let attempts = attempt_chain_for_request_for_node(store, request_id, node_did);
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

fn attempt_chain_for_request_for_node(
    store: &ClientStore,
    request_id: &str,
    node_did: &str,
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
                && row.node_did.as_deref().is_none_or(|did| did == node_did)
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
