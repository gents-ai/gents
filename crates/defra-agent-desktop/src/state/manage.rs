#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageSection {
    Behaviors,
    Backends,
    ToolSelections,
    InferenceProfiles,
    ScheduledTasks,
    RequestTimeline,
    RecentFailures,
}

impl ManageSection {
    pub const MANAGE: [Self; 5] = [
        Self::Behaviors,
        Self::Backends,
        Self::ToolSelections,
        Self::InferenceProfiles,
        Self::ScheduledTasks,
    ];

    pub const INSPECT: [Self; 2] = [Self::RequestTimeline, Self::RecentFailures];

    pub fn label(self) -> &'static str {
        match self {
            Self::Behaviors => "Behaviors",
            Self::Backends => "Backends",
            Self::ToolSelections => "Tool Selections",
            Self::InferenceProfiles => "Inference Profiles",
            Self::ScheduledTasks => "Scheduled Tasks",
            Self::RequestTimeline => "Request Timeline",
            Self::RecentFailures => "Recent Failures",
        }
    }

    pub fn supports_new_documents(self) -> bool {
        matches!(
            self,
            Self::Behaviors
                | Self::Backends
                | Self::ToolSelections
                | Self::InferenceProfiles
                | Self::ScheduledTasks
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorDraft {
    pub behavior_id: String,
    pub agent_did: String,
    pub display_name: String,
    pub system_prompt: String,
    pub backend_id: String,
    pub model_name: String,
    pub tool_selection_id: String,
    pub inference_profile_id: String,
    pub compaction_strategy: String,
    pub compaction_threshold: String,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackendDraft {
    pub backend_id: String,
    pub name: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub api_key: String,
    pub api_key_env_var: String,
    pub max_concurrent: String,
    pub max_queue_depth: String,
    pub enabled: bool,
    pub models: String,
    pub probe_status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSelectionDraft {
    pub selection_id: String,
    pub agent_did: String,
    pub display_name: String,
    pub enable_file_tools: bool,
    pub file_tools_mode: String,
    pub enable_bash: bool,
    pub bash_mode: String,
    pub cli_tool_names: String,
    pub enable_meta_tools: bool,
    pub delegate_to: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InferenceProfileDraft {
    pub profile_id: String,
    pub display_name: String,
    pub context_window: String,
    pub max_output_tokens: String,
    pub max_turns: String,
    pub temperature: String,
    pub stream_batch_ms: String,
    pub deadline_duration_secs: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTaskDraft {
    pub task_id: String,
    pub agent_did: String,
    pub behavior_id: String,
    pub name: String,
    pub prompt: String,
    pub interval_secs: String,
    pub enabled: bool,
    pub next_run_at: String,
    pub last_run_at: String,
    pub last_status: String,
    pub last_error: String,
    pub run_count: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManageDraft {
    Behavior(BehaviorDraft),
    Backend(BackendDraft),
    ToolSelection(ToolSelectionDraft),
    InferenceProfile(InferenceProfileDraft),
    ScheduledTask(ScheduledTaskDraft),
}

impl ManageDraft {
    pub fn entity_id(&self) -> &str {
        match self {
            Self::Behavior(draft) => &draft.behavior_id,
            Self::Backend(draft) => &draft.backend_id,
            Self::ToolSelection(draft) => &draft.selection_id,
            Self::InferenceProfile(draft) => &draft.profile_id,
            Self::ScheduledTask(draft) => &draft.task_id,
        }
    }

    pub fn section(&self) -> ManageSection {
        match self {
            Self::Behavior(_) => ManageSection::Behaviors,
            Self::Backend(_) => ManageSection::Backends,
            Self::ToolSelection(_) => ManageSection::ToolSelections,
            Self::InferenceProfile(_) => ManageSection::InferenceProfiles,
            Self::ScheduledTask(_) => ManageSection::ScheduledTasks,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageDraftOrigin {
    ExistingEntity(String),
    NewDocument,
}

#[derive(Debug, Clone)]
pub struct ManageState {
    pub selected_peer_id: Option<String>,
    pub selected_agent_did: Option<String>,
    pub selected_section: ManageSection,
    pub selected_entity_id: Option<String>,
    pub draft_origin: Option<ManageDraftOrigin>,
    pub draft: Option<ManageDraft>,
    pub last_apply_error: Option<String>,
}

impl Default for ManageState {
    fn default() -> Self {
        Self {
            selected_peer_id: None,
            selected_agent_did: None,
            selected_section: ManageSection::Behaviors,
            selected_entity_id: None,
            draft_origin: None,
            draft: None,
            last_apply_error: None,
        }
    }
}
