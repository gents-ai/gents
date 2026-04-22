#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageSection {
    Behaviors,
    Backends,
    ToolSelections,
    InferenceProfiles,
    Tasks,
    Schedules,
    EventTriggers,
    RequestTimeline,
    RecentFailures,
}

impl ManageSection {
    pub const MANAGE: [Self; 7] = [
        Self::Behaviors,
        Self::Backends,
        Self::ToolSelections,
        Self::InferenceProfiles,
        Self::Tasks,
        Self::Schedules,
        Self::EventTriggers,
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
            Self::EventTriggers => "Event Triggers",
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
                | Self::EventTriggers
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

/// Draft shown in the EventTrigger manage section.
///
/// Apply-owned fields are editable; runtime-owned fields
/// (`last_attempt_at`, `last_fired_source_doc_id`, `last_status`,
/// `last_error`, `fire_count`) are shown read-only in the editor so an
/// operator can see the last fire outcome without the apply path ever
/// overwriting them.
#[derive(Debug, Clone, PartialEq)]
pub struct EventTriggerDraft {
    pub trigger_id: String,
    pub task_id: String,
    pub source_collection: String,
    pub event_kind: String,
    pub filter: String,
    pub enabled: bool,
    pub concurrency: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_attempt_at: String,
    pub last_fired_source_doc_id: String,
    pub last_status: String,
    pub last_error: String,
    pub fire_count: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManageDraft {
    Behavior(BehaviorDraft),
    Backend(BackendDraft),
    ToolSelection(ToolSelectionDraft),
    InferenceProfile(InferenceProfileDraft),
    Task(TaskDraft),
    Schedule(ScheduleDraft),
    EventTrigger(EventTriggerDraft),
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
            Self::EventTrigger(draft) => &draft.trigger_id,
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
            Self::EventTrigger(_) => ManageSection::EventTriggers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManageDraftOrigin {
    ExistingEntity(String),
    NewDocument,
}

/// In-progress manual-run args editor state.
///
/// Populated when the operator clicks "Run Now" on a Task; cleared on
/// successful submit or Cancel. `args_text` is the multi-line JSON the
/// user is typing; it is parsed only at submit time so the operator can
/// land an invalid draft temporarily without losing their keystrokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireTaskDraft {
    pub task_id: String,
    /// Multi-line JSON text the user is editing.
    pub args_text: String,
    /// Last submit error, if any (parsing or mutation failure).
    pub error: Option<String>,
}

impl FireTaskDraft {
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            args_text: "{}".to_string(),
            error: None,
        }
    }
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
    /// Populated while the operator is editing manual-run args in the
    /// Task editor's "Run Now" modal. `None` means the modal is closed.
    pub fire_task_draft: Option<FireTaskDraft>,
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
            fire_task_draft: None,
        }
    }
}
