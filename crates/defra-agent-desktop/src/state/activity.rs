#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Chat,
    Manage,
}

impl Activity {
    pub fn rail_width(self) -> Option<f32> {
        match self {
            Self::Chat => None,
            Self::Manage => Some(400.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingChatAction {
    SelectDeployment { peer_id: String, agent_did: String },
    SelectConversation { session_id: String },
    StartNewConversationDraft,
    SelectBehavior { behavior_id: Option<String> },
    CreateConversation,
    SubmitComposer,
    RetryLatestRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingManageAction {
    SelectDeployment {
        peer_id: String,
        agent_did: String,
    },
    SelectSection {
        section: super::ManageSection,
    },
    SelectEntity {
        entity_id: String,
    },
    StartNewDocument,
    DiscardDraft,
    ApplyDraft,
    RunNowSelectedTask,
    /// Submit the in-progress `fire_task_draft`. The controller parses
    /// the JSON args, looks the `TaskRow` up in the store, and calls
    /// `ClientCore::fire_task_now`. On success the modal closes.
    SubmitFireTaskDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingShellAction {
    Navigate(Activity),
    OpenDeploymentSetup,
    SelectScopedDeployment { peer_id: String, agent_did: String },
    Chat(PendingChatAction),
    Manage(PendingManageAction),
}
