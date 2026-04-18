use std::mem;

use super::status::IdentityState;
use super::{
    Activity, ChatEditorState, ChatState, LogsState, OnboardingState, ManageState, SetupState,
    PendingShellAction, StatusBarState,
};

#[derive(Debug, Clone)]
pub struct ShellState {
    pub activity: Activity,
    pub chat: ChatState,
    pub setup: SetupState,
    pub manage: ManageState,
    pub logs: LogsState,
    pub onboarding: OnboardingState,
    pub pending_shell_actions: Vec<PendingShellAction>,
    pub pending_client_restart_reason: Option<String>,
    pub identity: IdentityState,
    pub status: StatusBarState,
}

impl ShellState {
    pub fn queue_shell_action(&mut self, action: PendingShellAction) {
        self.pending_shell_actions.push(action);
    }

    pub fn drain_pending_shell_actions(&mut self) -> Vec<PendingShellAction> {
        mem::take(&mut self.pending_shell_actions)
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            activity: Activity::Chat,
            chat: ChatState {
                editor: ChatEditorState {
                    transcript_stick_to_bottom: true,
                    ..ChatEditorState::default()
                },
                ..ChatState::default()
            },
            setup: SetupState::default(),
            manage: ManageState::default(),
            logs: LogsState::default(),
            onboarding: OnboardingState::default(),
            pending_shell_actions: Vec::new(),
            pending_client_restart_reason: None,
            identity: IdentityState {
                initials: "D1",
                label: "FIELD PRINCIPAL".to_string(),
                did_short: "did:defra:9v6q..p0ra".to_string(),
            },
            status: StatusBarState {
                peered_now: 2,
                peered_target: 3,
                p2p_state: "healthy".to_string(),
                p2p_warning: false,
                active_agent: "amy-code".to_string(),
                runtime_state: "observing".to_string(),
                gossip_lag_ms: 84,
                replication_state: "converged".to_string(),
                error_count: 0,
                frame_counter: 4021,
                did_short: "did:defra:9v6q..p0ra".to_string(),
                build_label: "shell-t2".to_string(),
            },
        }
    }
}
