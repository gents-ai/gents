use defra_agent_protocol::client_protocol::ClientTurnState;

use crate::chat::domain::submission::{ChatBlockedReason, ChatWorkflowState};
use crate::client::ClientStore;
use crate::state::ChatState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionObservation {
    has_conversation: bool,
    has_session_row: bool,
    has_requests: bool,
    has_responses: bool,
    has_messages: bool,
    has_tool_calls: bool,
    has_tool_results: bool,
}

impl SessionObservation {
    pub(super) fn is_observed(self) -> bool {
        self.has_conversation
            || self.has_session_row
            || self.has_requests
            || self.has_responses
            || self.has_messages
            || self.has_tool_calls
            || self.has_tool_results
    }

    pub(super) fn has_turn_rows(self) -> bool {
        self.has_requests || self.has_responses
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RequestContext {
    pub(super) observation: SessionObservation,
    pub(super) active_request_id: Option<String>,
    pub(super) observed_request_ids: Vec<String>,
    pub(super) turn_state: Option<ClientTurnState>,
    pub(super) behavior_mismatch: Option<ChatBlockedReason>,
    pub(super) targets_selected_session: bool,
}

impl RequestContext {
    pub(super) fn turn_state_for_request(&self, request_id: &str) -> Option<ClientTurnState> {
        if self
            .active_request_id
            .as_deref()
            .is_some_and(|active_request_id| active_request_id == request_id)
        {
            return self.turn_state;
        }

        None
    }
}

pub(super) fn request_context(
    state: &ChatState,
    store: &ClientStore,
    selected_session_id: Option<&str>,
    selected_agent_did: Option<&str>,
) -> RequestContext {
    let Some(session_id) = selected_session_id else {
        return empty_request_context();
    };

    let observation = observe_session(store, session_id);
    let tracked_request_id = tracked_request_id_for_session(&state.shell.workflow, session_id);
    let observed_request_ids = store
        .requests_for_session(session_id)
        .iter()
        .map(|row| row.request_id.clone())
        .collect::<Vec<_>>();
    let active_request_id = tracked_request_id
        .filter(|request_id| {
            observed_request_ids
                .iter()
                .any(|observed| observed == request_id)
        })
        .or_else(|| store.latest_request_id_for_session(session_id));
    let turn_state = active_request_id
        .as_deref()
        .and_then(|request_id| store.derive_turn_for_request(request_id))
        .or_else(|| store.derive_turn(session_id));
    let behavior_mismatch = selected_agent_did.and_then(|agent_did| {
        session_behavior_mismatch(
            state.editor.selected_behavior_override.as_deref(),
            store,
            session_id,
            agent_did,
        )
    });

    RequestContext {
        observation,
        active_request_id,
        observed_request_ids,
        turn_state,
        behavior_mismatch,
        targets_selected_session: true,
    }
}

pub(super) fn session_trustworthy_for_follow_up(
    request_context: &RequestContext,
    selected_session_id: Option<&str>,
) -> bool {
    selected_session_id.is_none()
        || (request_context.observation.is_observed()
            && (!request_context.observation.has_turn_rows()
                || request_context.turn_state.is_some())
            && request_context.behavior_mismatch.is_none())
}

pub(super) fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn empty_request_context() -> RequestContext {
    RequestContext {
        observation: SessionObservation {
            has_conversation: false,
            has_session_row: false,
            has_requests: false,
            has_responses: false,
            has_messages: false,
            has_tool_calls: false,
            has_tool_results: false,
        },
        active_request_id: None,
        observed_request_ids: Vec::new(),
        turn_state: None,
        behavior_mismatch: None,
        targets_selected_session: false,
    }
}

fn tracked_request_id_for_session(
    workflow: &ChatWorkflowState,
    session_id: &str,
) -> Option<String> {
    match workflow {
        ChatWorkflowState::AwaitingObservation {
            session_id: tracked_session_id,
            request_id,
        } if tracked_session_id == session_id => Some(request_id.clone()),
        ChatWorkflowState::TurnInProgress {
            session_id: tracked_session_id,
            request_id,
            ..
        } if tracked_session_id == session_id => request_id.clone(),
        _ => None,
    }
}

fn observe_session(store: &ClientStore, session_id: &str) -> SessionObservation {
    let transcript = store.transcript(session_id);
    SessionObservation {
        has_conversation: store
            .conversations
            .iter()
            .any(|row| row.session_id == session_id),
        has_session_row: store
            .sessions
            .iter()
            .any(|row| row.session_id == session_id),
        has_requests: !store.requests_for_session(session_id).is_empty(),
        has_responses: store
            .responses
            .iter()
            .any(|row| row.session_id.as_deref() == Some(session_id)),
        has_messages: !transcript.messages.is_empty(),
        has_tool_calls: !transcript.tool_calls.is_empty(),
        has_tool_results: !transcript.tool_results.is_empty(),
    }
}

fn session_behavior_mismatch(
    requested_behavior_id: Option<&str>,
    store: &ClientStore,
    session_id: &str,
    agent_did: &str,
) -> Option<ChatBlockedReason> {
    let requested = normalize_optional_string(requested_behavior_id)?;
    let existing = store
        .conversations
        .iter()
        .find(|row| row.session_id == session_id && row.agent_did.as_deref() == Some(agent_did))
        .and_then(|row| normalize_optional_string(row.behavior_id.as_deref()))
        .or_else(|| {
            store
                .sessions
                .iter()
                .find(|row| row.session_id == session_id)
                .and_then(|row| normalize_optional_string(row.behavior_id.as_deref()))
        })?;

    if existing == requested {
        return None;
    }

    Some(ChatBlockedReason::SessionBehaviorMismatch {
        requested,
        existing,
    })
}
