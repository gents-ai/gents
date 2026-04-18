use crate::chat::controller as chat_controller;
use crate::manage::controller as manage_controller;
use crate::state::{Activity, PendingChatAction, PendingManageAction, PendingShellAction};

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
            PendingShellAction::OpenDeploymentSetup => {
                self.state.activity = Activity::Chat;
                self.state.setup.workspace_open = true;
                self.state.setup.show_add_form = true;
            }
            PendingShellAction::SelectScopedDeployment { peer_id, agent_did } => {
                chat_controller::select_deployment(
                    &mut self.state.chat,
                    peer_id.clone(),
                    agent_did.clone(),
                );
                manage_controller::select_deployment(
                    &mut self.state.manage,
                    peer_id.clone(),
                    agent_did,
                );
            }
            PendingShellAction::Chat(action) => self.process_pending_chat_action(action),
            PendingShellAction::Manage(action) => self.process_pending_manage_action(action),
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

    fn process_pending_manage_action(&mut self, action: PendingManageAction) {
        match action {
            PendingManageAction::SelectDeployment { peer_id, agent_did } => {
                manage_controller::select_deployment(&mut self.state.manage, peer_id, agent_did);
            }
            PendingManageAction::SelectSection { section } => {
                manage_controller::select_section(&mut self.state.manage, section);
            }
            PendingManageAction::SelectEntity { entity_id } => {
                manage_controller::select_entity(&mut self.state.manage, entity_id);
            }
            PendingManageAction::StartNewDocument => {
                manage_controller::start_new_document(&mut self.state.manage);
            }
            PendingManageAction::DiscardDraft => {
                if let Some(client) = self.client.as_deref() {
                    let snapshot = client.store().snapshot();
                    manage_controller::discard_draft(
                        &mut self.state.manage,
                        &client.peer_statuses(),
                        snapshot.as_ref(),
                    );
                } else {
                    self.state.manage.last_apply_error = Some("client core is offline".to_string());
                }
            }
            PendingManageAction::ApplyDraft => {
                if let Err(error) = manage_controller::apply_draft(
                    &mut self.state.manage,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.manage.last_apply_error = Some(error.to_string());
                }
            }
            PendingManageAction::RunNowSelectedTask => {
                if let Err(error) = manage_controller::run_selected_task_now(
                    &mut self.state.manage,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.manage.last_apply_error = Some(error.to_string());
                }
            }
        }
    }
}
