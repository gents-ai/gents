use crate::chat::controller as chat_controller;
use crate::operator::controller as operator_controller;
use crate::state::{Activity, PendingChatAction, PendingOperatorAction, PendingShellAction};

use super::DesktopApp;

impl DesktopApp {
    pub(super) fn process_pending_shell_actions(&mut self) {
        let pending_actions = self.state.drain_pending_shell_actions();
        for action in pending_actions {
            self.process_pending_shell_action(action);
        }
    }

    fn process_pending_shell_action(&mut self, action: PendingShellAction) {
        match action {
            PendingShellAction::Navigate(activity) => {
                self.state.activity = activity;
            }
            PendingShellAction::OpenPeersSetup => {
                self.state.activity = Activity::Peers;
                self.state.peers.show_add_form = true;
            }
            PendingShellAction::SelectScopedDeployment { peer_id, agent_did } => {
                chat_controller::select_deployment(
                    &mut self.state.chat,
                    peer_id.clone(),
                    agent_did.clone(),
                );
                operator_controller::select_deployment(
                    &mut self.state.operator,
                    peer_id.clone(),
                    agent_did,
                );
            }
            PendingShellAction::Chat(action) => self.process_pending_chat_action(action),
            PendingShellAction::Operator(action) => self.process_pending_operator_action(action),
        }
    }

    fn process_pending_chat_action(&mut self, action: PendingChatAction) {
        match action {
            PendingChatAction::SelectDeployment { peer_id, agent_did } => {
                chat_controller::select_deployment(&mut self.state.chat, peer_id, agent_did);
            }
            PendingChatAction::SelectConversation { session_id } => {
                chat_controller::select_conversation(&mut self.state.chat, session_id);
            }
            PendingChatAction::StartNewConversationDraft => {
                chat_controller::start_new_conversation_draft(&mut self.state.chat);
            }
            PendingChatAction::SelectBehavior { behavior_id } => {
                chat_controller::select_behavior_override(&mut self.state.chat, behavior_id);
            }
            PendingChatAction::CreateConversation => {
                if let Err(error) = chat_controller::create_conversation(
                    &mut self.state.chat,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.chat.editor.last_submission_error = Some(error.to_string());
                }
            }
            PendingChatAction::SubmitComposer => {
                if let Err(error) = chat_controller::submit_composer(
                    &mut self.state.chat,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.chat.editor.last_submission_error = Some(error.to_string());
                }
            }
            PendingChatAction::RetryLatestRequest => {
                let latest_request = self.client.as_ref().and_then(|client| {
                    let session_id = self.state.chat.shell.selected_session_id.as_deref()?;
                    let snapshot = client.store().snapshot();
                    snapshot
                        .requests_for_session(session_id)
                        .into_iter()
                        .last()
                        .cloned()
                });

                match chat_controller::retry_latest_request(
                    &mut self.state.chat,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                    latest_request.as_ref(),
                ) {
                    Ok(()) => {
                        self.state.chat.editor.last_action_message =
                            Some("Retried latest request.".to_string());
                    }
                    Err(error) => {
                        self.state.chat.editor.last_submission_error = Some(error.to_string());
                    }
                }
            }
        }
    }

    fn process_pending_operator_action(&mut self, action: PendingOperatorAction) {
        match action {
            PendingOperatorAction::SelectDeployment { peer_id, agent_did } => {
                operator_controller::select_deployment(
                    &mut self.state.operator,
                    peer_id,
                    agent_did,
                );
            }
            PendingOperatorAction::SelectSection { section } => {
                operator_controller::select_section(&mut self.state.operator, section);
            }
            PendingOperatorAction::SelectEntity { entity_id } => {
                operator_controller::select_entity(&mut self.state.operator, entity_id);
            }
            PendingOperatorAction::StartNewDocument => {
                operator_controller::start_new_document(&mut self.state.operator);
            }
            PendingOperatorAction::DiscardDraft => {
                if let Some(client) = self.client.as_deref() {
                    let snapshot = client.store().snapshot();
                    operator_controller::discard_draft(
                        &mut self.state.operator,
                        &client.peer_statuses(),
                        snapshot.as_ref(),
                    );
                } else {
                    self.state.operator.last_apply_error =
                        Some("client core is offline".to_string());
                }
            }
            PendingOperatorAction::ApplyDraft => {
                if let Err(error) = operator_controller::apply_draft(
                    &mut self.state.operator,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.operator.last_apply_error = Some(error.to_string());
                }
            }
            PendingOperatorAction::RunNowSelectedTask => {
                if let Err(error) = operator_controller::run_selected_task_now(
                    &mut self.state.operator,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.operator.last_apply_error = Some(error.to_string());
                }
            }
        }
    }
}
