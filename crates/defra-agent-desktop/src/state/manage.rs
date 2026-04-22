#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageSection {
    Behaviors,
    Backends,
    ToolSelections,
    InferenceProfiles,
    Tasks,
    Schedules,
    RequestTimeline,
    RecentFailures,
}

impl ManageSection {
    pub const MANAGE: [Self; 6] = [
        Self::Behaviors,
        Self::Backends,
        Self::ToolSelections,
        Self::InferenceProfiles,
        Self::Tasks,
        Self::Schedules,
    ];

    pub const INSPECT: [Self; 2] = [Self::RequestTimeline, Self::RecentFailures];

    pub fn label(self) -> &'static str {
        match self {
            Self::Behaviors => "Behaviors",
            Self::Backends => "Backends",
            Self::ToolSelections => "Tool Selections",
            Self::InferenceProfiles => "Inference Profiles",
            Self::Tasks => "Tasks",
            Self::Schedules => "Schedules",
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
                | Self::Tasks
                | Self::Schedules
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

/// Read-only draft shown in the Task manage section.
///
/// Task 51 renders the task list; Task 52 will replace this draft with a
/// full editor (description, prompt_template, output_schema_ref, etc).
#[derive(Debug, Clone, PartialEq)]
pub struct TaskDraft {
    pub task_id: String,
    pub name: String,
    pub description: String,
    pub behavior_id: String,
    pub prompt_template: String,
    pub enabled: bool,
    pub output_schema_ref: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Read-only draft shown in the Schedule manage section.
///
/// Task 51 renders the schedule list; Task 52 will wire up a real editor
/// with mutations, and Task 53 will surface the fire-bookkeeping fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleDraft {
    pub schedule_id: String,
    pub task_id: String,
    pub interval_secs: String,
    pub enabled: bool,
    pub concurrency: String,
    pub next_run_at: String,
    pub last_attempt_at: String,
    pub last_status: String,
    pub last_error: String,
    pub fire_count: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManageDraft {
    Behavior(BehaviorDraft),
    Backend(BackendDraft),
    ToolSelection(ToolSelectionDraft),
    InferenceProfile(InferenceProfileDraft),
    Task(TaskDraft),
    Schedule(ScheduleDraft),
}

impl ManageDraft {
    pub fn entity_id(&self) -> &str {
        match self {
            Self::Behavior(draft) => &draft.behavior_id,
            Self::Backend(draft) => &draft.backend_id,
            Self::ToolSelection(draft) => &draft.selection_id,
            Self::InferenceProfile(draft) => &draft.profile_id,
            Self::Task(draft) => &draft.task_id,
            Self::Schedule(draft) => &draft.schedule_id,
        }
    }

    pub fn section(&self) -> ManageSection {
        match self {
            Self::Behavior(_) => ManageSection::Behaviors,
            Self::Backend(_) => ManageSection::Backends,
            Self::ToolSelection(_) => ManageSection::ToolSelections,
            Self::InferenceProfile(_) => ManageSection::InferenceProfiles,
            Self::Task(_) => ManageSection::Tasks,
            Self::Schedule(_) => ManageSection::Schedules,
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
