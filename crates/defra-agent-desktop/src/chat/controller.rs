use anyhow::{Context, Result};
use defra_agent_protocol::row::AgentRequestRow;
use tokio::runtime::Runtime;

use crate::chat::actions::{reduce, ChatAction};
use crate::chat::domain::submission::ChatBlockedReason;
use crate::chat::projection::{project_chat, ChatProjection};
use crate::client::{ClientCore, ClientStore};
use crate::state::ChatState;

pub fn sync_from_snapshot(
    chat: &mut ChatState,
    store: &ClientStore,
    client_available: bool,
) -> ChatProjection {
    let projection = project_chat(chat, store, client_available);
    reduce(
        chat,
        ChatAction::SnapshotWorkflowApplied {
            workflow: projection.workflow.clone(),
        },
    );
    projection
}

pub fn select_deployment(chat: &mut ChatState, peer_id: String, agent_did: String) {
    reduce(chat, ChatAction::SelectDeployment { peer_id, agent_did });
}

pub fn select_conversation(chat: &mut ChatState, session_id: String) {
    reduce(chat, ChatAction::SelectConversation { session_id });
}

pub fn start_new_conversation_draft(chat: &mut ChatState) {
    reduce(chat, ChatAction::StartNewConversationDraft);
}

pub fn select_behavior_override(chat: &mut ChatState, behavior_id: Option<String>) {
    reduce(chat, ChatAction::SelectBehaviorOverride { behavior_id });
}

pub fn create_conversation(
    chat: &mut ChatState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let agent_did = chat
        .shell
        .selected_agent_did
        .as_deref()
        .context("select an agent before creating a conversation")?
        .to_string();
    reduce(
        chat,
        ChatAction::CreateConversationStarted {
            agent_did: agent_did.clone(),
        },
    );

    let behavior_override = chat.editor.selected_behavior_override.clone();
    match runtime.block_on(client.create_conversation(&agent_did, behavior_override.as_deref())) {
        Ok(created) => {
            reduce(
                chat,
                ChatAction::ConversationCreated {
                    session_id: created.session_id,
                },
            );
            let snapshot = client.store().snapshot();
            sync_from_snapshot(chat, snapshot.as_ref(), true);
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                chat,
                ChatAction::MutationFailed {
                    blocked_reason: classify_mutation_error(&error_text),
                    error: error_text.clone(),
                },
            );
            Err(anyhow::anyhow!(error_text))
        }
    }
}

pub fn submit_composer(
    chat: &mut ChatState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let agent_did = chat
        .shell
        .selected_agent_did
        .as_deref()
        .context("select an agent before sending")?
        .to_string();

    let behavior_override = chat.editor.selected_behavior_override.clone();
    let content = chat.editor.composer_text.clone();
    let session_id = chat.shell.selected_session_id.clone();

    let submission = if let Some(session_id) = session_id {
        reduce(
            chat,
            ChatAction::SubmitRequestStarted {
                agent_did: agent_did.clone(),
                session_id: Some(session_id.clone()),
            },
        );
        runtime.block_on(client.submit_request(
            &session_id,
            &agent_did,
            &content,
            behavior_override.as_deref(),
        ))
    } else {
        reduce(
            chat,
            ChatAction::CreateConversationStarted {
                agent_did: agent_did.clone(),
            },
        );
        let created = match runtime
            .block_on(client.create_conversation(&agent_did, behavior_override.as_deref()))
        {
            Ok(created) => created,
            Err(error) => {
                let error_text = error.to_string();
                reduce(
                    chat,
                    ChatAction::MutationFailed {
                        blocked_reason: classify_mutation_error(&error_text),
                        error: error_text.clone(),
                    },
                );
                return Err(anyhow::anyhow!(error_text));
            }
        };
        reduce(
            chat,
            ChatAction::ConversationCreated {
                session_id: created.session_id.clone(),
            },
        );
        reduce(
            chat,
            ChatAction::SubmitRequestStarted {
                agent_did: agent_did.clone(),
                session_id: Some(created.session_id.clone()),
            },
        );
        runtime.block_on(client.submit_request(
            &created.session_id,
            &agent_did,
            &content,
            behavior_override.as_deref(),
        ))
    };

    match submission {
        Ok(submitted) => {
            reduce(
                chat,
                ChatAction::RequestSubmitted {
                    session_id: submitted.session_id,
                    request_id: submitted.request_id,
                },
            );
            let snapshot = client.store().snapshot();
            sync_from_snapshot(chat, snapshot.as_ref(), true);
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                chat,
                ChatAction::MutationFailed {
                    blocked_reason: classify_mutation_error(&error_text),
                    error: error_text.clone(),
                },
            );
            Err(anyhow::anyhow!(error_text))
        }
    }
}

pub fn retry_latest_request(
    chat: &mut ChatState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
    request: Option<&AgentRequestRow>,
) -> Result<()> {
    let client = client.context("client core is offline")?;
    let request = request.context("no request is available to retry")?;
    let agent_did = request
        .agent_did
        .as_deref()
        .context("retry parent request must have an agent_did")?;
    let session_id = request
        .session_id
        .as_deref()
        .context("retry parent request must have a session_id")?;

    reduce(
        chat,
        ChatAction::RetryStarted {
            agent_did: agent_did.to_string(),
            session_id: session_id.to_string(),
        },
    );

    match runtime.block_on(client.retry_request(request)) {
        Ok(submitted) => {
            reduce(
                chat,
                ChatAction::RetrySubmitted {
                    session_id: submitted.session_id,
                    request_id: submitted.request_id,
                },
            );
            chat.editor.last_action_message = Some("Retried latest request.".to_string());
            let snapshot = client.store().snapshot();
            sync_from_snapshot(chat, snapshot.as_ref(), true);
            Ok(())
        }
        Err(error) => {
            let error_text = error.to_string();
            reduce(
                chat,
                ChatAction::MutationFailed {
                    blocked_reason: classify_mutation_error(&error_text),
                    error: error_text.clone(),
                },
            );
            Err(anyhow::anyhow!(error_text))
        }
    }
}

fn classify_mutation_error(error: &str) -> Option<ChatBlockedReason> {
    if let Some((existing, requested)) = parse_behavior_mismatch(error) {
        return Some(ChatBlockedReason::SessionBehaviorMismatch {
            requested,
            existing,
        });
    }
    None
}

fn parse_behavior_mismatch(error: &str) -> Option<(String, String)> {
    let existing_marker = "existing=";
    let requested_marker = "requested=";
    let existing_index = error.find(existing_marker)?;
    let requested_index = error.find(requested_marker)?;
    let existing = error[existing_index + existing_marker.len()..]
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .to_string();
    let requested = error[requested_index + requested_marker.len()..]
        .split_whitespace()
        .next()?
        .trim_end_matches(',')
        .to_string();
    Some((existing, requested))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::chat::domain::submission::{ChatWorkflowState, SendStatus};
    use crate::client::ClientStoreRows;
    use defra_agent_protocol::row::{
        AgentConversationRow, AgentPrincipalRow, AgentRequestRow, AgentResponseRow,
    };

    #[test]
    fn sync_transitions_awaiting_observation_into_turn_in_progress() {
        let mut chat = ChatState {
            shell: crate::state::ChatShellState {
                selected_agent_did: Some("did:defra:amy".to_string()),
                selected_session_id: Some("session-1".to_string()),
                workflow: ChatWorkflowState::AwaitingObservation {
                    session_id: "session-1".to_string(),
                    request_id: "req-1".to_string(),
                },
                ..crate::state::ChatShellState::default()
            },
            editor: crate::state::ChatEditorState {
                composer_text: "follow up".to_string(),
                ..crate::state::ChatEditorState::default()
            },
            ..ChatState::default()
        };
        let store = ClientStore::from_rows(ClientStoreRows {
            agent_principals: vec![AgentPrincipalRow {
                agent_did: "did:defra:amy".to_string(),
                display_name: Some("Amy".to_string()),
                default_behavior_id: Some("amy-default".to_string()),
                enabled: Some(true),
                created_at: None,
                created_by: None,
            }],
            conversations: vec![AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("Session".to_string()),
                title_source: Some("generated".to_string()),
                preview_text: None,
                status: Some("active".to_string()),
                created_at: Some("2026-04-14T00:00:00Z".to_string()),
                updated_at: Some("2026-04-14T00:05:00Z".to_string()),
                latest_request_id: Some("req-stale".to_string()),
            }],
            requests: vec![AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("hello".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: None,
                failure_reason: None,
                created_at: Some("2026-04-14T00:00:01Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            }],
            responses: vec![AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-1".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: None,
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: None,
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-14T00:00:02Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            }],
            ..ClientStoreRows::default()
        });

        let projection = sync_from_snapshot(&mut chat, &store, true);

        assert_eq!(
            projection.send_status,
            SendStatus::Disabled(ChatBlockedReason::AwaitingTurnTerminality(
                defra_agent_protocol::client_protocol::ClientTurnState::Streaming,
            ))
        );
        assert!(matches!(
            chat.shell.workflow,
            ChatWorkflowState::TurnInProgress { .. }
        ));
    }
}
