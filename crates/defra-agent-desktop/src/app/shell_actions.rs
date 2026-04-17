use crate::chat::controller as chat_controller;
use crate::state::{Activity, PendingChatAction, PendingShellAction};

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
            PendingShellAction::Chat(action) => self.process_pending_chat_action(action),
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
}
