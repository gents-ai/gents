use defra_agent_protocol::client_protocol::ClientTurnState;

use crate::chat::domain::submission::{ChatWorkflowState, SendStatus};
use crate::client::ClientStore;
use crate::state::ChatState;

mod context;
mod send_status;
#[cfg(test)]
mod tests;
mod workflow;

use context::{request_context, session_trustworthy_for_follow_up};
use send_status::project_send_status;
use workflow::project_workflow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatProjection {
    pub turn_state: Option<ClientTurnState>,
    pub send_status: SendStatus,
    pub show_first_conversation_nudge: bool,
    pub session_trustworthy_for_follow_up: bool,
    pub workflow: ChatWorkflowState,
}

pub fn project_chat(
    state: &ChatState,
    store: &ClientStore,
    client_available: bool,
) -> ChatProjection {
    let selected_agent_did = state.shell.selected_agent_did.as_deref();
    let selected_session_id = state.shell.selected_session_id.as_deref();

    let request_context = request_context(state, store, selected_session_id, selected_agent_did);
    let show_first_conversation_nudge = selected_agent_did.is_some_and(|agent_did| {
        selected_session_id.is_none() && store.conversation_rows(agent_did).is_empty()
    });
    let session_trustworthy_for_follow_up =
        session_trustworthy_for_follow_up(&request_context, selected_session_id);
    let send_status = project_send_status(
        state,
        client_available,
        selected_agent_did,
        selected_session_id,
        &request_context,
    );
    let workflow = project_workflow(
        &state.shell.workflow,
        &request_context,
        selected_session_id,
        selected_agent_did,
        client_available,
    );

    ChatProjection {
        turn_state: request_context.turn_state,
        send_status,
        show_first_conversation_nudge,
        session_trustworthy_for_follow_up,
        workflow,
    }
}
