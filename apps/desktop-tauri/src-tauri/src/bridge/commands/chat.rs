use anyhow::{bail, Result};
use defra_agent_desktop_core::client::ClientCore;
use defra_agent_protocol::client_protocol::ClientTurnState;
use uuid::Uuid;

use super::super::types::{ChatSendRequest, ChatSendResult, ConversationRenameRequest};

fn can_send_in_turn(state: ClientTurnState) -> bool {
    matches!(
        state,
        ClientTurnState::Completed
            | ClientTurnState::Failed
            | ClientTurnState::Superseded
            | ClientTurnState::Interrupted
    )
}

fn turn_state_label(state: ClientTurnState) -> &'static str {
    match state {
        ClientTurnState::WaitingForClaim => "waitingForClaim",
        ClientTurnState::Streaming => "streaming",
        ClientTurnState::Completed => "completed",
        ClientTurnState::Failed => "failed",
        ClientTurnState::Superseded => "superseded",
        ClientTurnState::Interrupted => "interrupted",
    }
}

pub(crate) async fn send_chat_message(
    core: &ClientCore,
    request: ChatSendRequest,
) -> Result<ChatSendResult> {
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        bail!("agent_did is required");
    }

    let content = request.content.trim().to_string();
    if content.is_empty() {
        bail!("content is required");
    }

    let behavior_id = request
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let requested_session_id = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let remote_graphql = core.graphql_for_agent(&agent_did).await;
    let session_id = match requested_session_id {
        Some(session_id) => session_id.to_string(),
        None if remote_graphql.is_some() => Uuid::new_v4().to_string(),
        None => {
            core.create_conversation(&agent_did, behavior_id.as_deref())
                .await?
                .session_id
        }
    };

    let store = core.store().snapshot();
    if let Some(turn_state) = store.derive_turn(&session_id) {
        if !can_send_in_turn(turn_state) {
            bail!(
                "cannot send while current turn is {}",
                turn_state_label(turn_state)
            );
        }
    }

    let submitted = match remote_graphql {
        Some(graphql) => {
            core.submit_remote_graphql_request_with_options(
                &graphql,
                &session_id,
                &agent_did,
                &content,
                behavior_id.as_deref(),
                Default::default(),
            )
            .await?
        }
        None => {
            core.submit_request(&session_id, &agent_did, &content, behavior_id.as_deref())
                .await?
        }
    };

    Ok(ChatSendResult {
        session_id,
        request_id: submitted.request_id,
        agent_did: submitted.agent_did,
        behavior_id: submitted.behavior_id,
    })
}

pub(crate) async fn rename_conversation(
    core: &ClientCore,
    request: ConversationRenameRequest,
) -> Result<()> {
    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        bail!("session_id is required");
    }
    let title = request.title.trim().to_string();
    if title.is_empty() {
        bail!("title is required");
    }
    core.rename_conversation(&session_id, &title).await?;
    Ok(())
}
