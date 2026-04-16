use std::collections::BTreeSet;

use crate::telemetry::DesktopLogCategory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Chat,
    Operator,
    Peers,
    Logs,
}

impl Activity {
    pub const ALL: [Self; 4] = [Self::Chat, Self::Operator, Self::Peers, Self::Logs];

    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Operator => "Operator",
            Self::Peers => "Peers",
            Self::Logs => "Logs",
        }
    }

    pub fn nav_hint(self) -> &'static str {
        match self {
            Self::Chat => "conversations",
            Self::Operator => "config + runtime",
            Self::Peers => "pairing + identity",
            Self::Logs => "diagnostics",
        }
    }

    pub fn nav_badge(self) -> &'static str {
        match self {
            Self::Chat => "CH",
            Self::Operator => "OP",
            Self::Peers => "PP",
            Self::Logs => "LG",
        }
    }

    pub fn sidebar_width(self) -> f32 {
        match self {
            Self::Chat => 308.0,
            Self::Operator | Self::Peers => 292.0,
            Self::Logs => 272.0,
        }
    }

    pub fn rail_width(self) -> Option<f32> {
        match self {
            Self::Chat => None,
            Self::Operator => Some(400.0),
            Self::Peers => Some(380.0),
            Self::Logs => Some(360.0),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatState {
    pub selected_peer_id: Option<String>,
    pub selected_agent_did: Option<String>,
    pub selected_session_id: Option<String>,
    pub auto_create_attempted_agent_did: Option<String>,
    pub composer_text: String,
    pub selected_behavior_override: Option<String>,
    pub expanded_tool_cards: BTreeSet<String>,
    pub expanded_reasoning_cards: BTreeSet<String>,
    pub transcript_stick_to_bottom: bool,
    pub last_submission_error: Option<String>,
    pub last_action_message: Option<String>,
    pub last_export_payload: Option<String>,
    pub tool_detail_modal: Option<ToolDetailModalState>,
    pub new_conversation_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDetailModalState {
    pub card_id: String,
    pub title: String,
    pub body: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PeersState {
    pub selected_peer_id: Option<String>,
    pub show_add_form: bool,
    pub add_label: String,
    pub add_addr: String,
    pub add_agent_did: String,
    pub last_action_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorSection {
    Runtime,
    Behaviors,
    Backends,
    ToolSelections,
    InferenceProfiles,
    ScheduledTasks,
    RequestTimeline,
    RecentFailures,
}

impl OperatorSection {
    pub const MANAGE: [Self; 6] = [
        Self::Runtime,
        Self::Behaviors,
        Self::Backends,
        Self::ToolSelections,
        Self::InferenceProfiles,
        Self::ScheduledTasks,
    ];

    pub const INSPECT: [Self; 2] = [Self::RequestTimeline, Self::RecentFailures];

    pub fn label(self) -> &'static str {
        match self {
            Self::Runtime => "Runtime",
            Self::Behaviors => "Behaviors",
            Self::Backends => "Backends",
            Self::ToolSelections => "Tool Selections",
            Self::InferenceProfiles => "Inference Profiles",
            Self::ScheduledTasks => "Scheduled Tasks",
            Self::RequestTimeline => "Request Timeline",
            Self::RecentFailures => "Recent Failures",
        }
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
pub enum OperatorDraft {
    Behavior(BehaviorDraft),
    Backend(BackendDraft),
    ToolSelection(ToolSelectionDraft),
    InferenceProfile(InferenceProfileDraft),
    ScheduledTask(ScheduledTaskDraft),
}

impl OperatorDraft {
    pub fn entity_id(&self) -> &str {
        match self {
            Self::Behavior(draft) => &draft.behavior_id,
            Self::Backend(draft) => &draft.backend_id,
            Self::ToolSelection(draft) => &draft.selection_id,
            Self::InferenceProfile(draft) => &draft.profile_id,
            Self::ScheduledTask(draft) => &draft.task_id,
        }
    }

    pub fn section(&self) -> OperatorSection {
        match self {
            Self::Behavior(_) => OperatorSection::Behaviors,
            Self::Backend(_) => OperatorSection::Backends,
            Self::ToolSelection(_) => OperatorSection::ToolSelections,
            Self::InferenceProfile(_) => OperatorSection::InferenceProfiles,
            Self::ScheduledTask(_) => OperatorSection::ScheduledTasks,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OperatorState {
    pub selected_peer_id: Option<String>,
    pub selected_agent_did: Option<String>,
    pub selected_section: OperatorSection,
    pub selected_entity_id: Option<String>,
    pub draft_source_entity_id: Option<String>,
    pub entity_filter: String,
    pub draft: Option<OperatorDraft>,
    pub last_apply_error: Option<String>,
}

impl Default for OperatorState {
    fn default() -> Self {
        Self {
            selected_peer_id: None,
            selected_agent_did: None,
            selected_section: OperatorSection::Behaviors,
            selected_entity_id: None,
            draft_source_entity_id: None,
            entity_filter: String::new(),
            draft: None,
            last_apply_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IdentityState {
    pub initials: &'static str,
    pub label: String,
    pub did_short: String,
}

#[derive(Debug, Clone)]
pub struct StatusBarState {
    pub peered_now: usize,
    pub peered_target: usize,
    pub active_agent: String,
    pub runtime_state: String,
    pub gossip_lag_ms: u32,
    pub replication_state: String,
    pub error_count: usize,
    pub frame_counter: u64,
    pub did_short: String,
    pub build_label: String,
}

impl StatusBarState {
    pub fn advance_frame(&mut self) {
        self.frame_counter = self.frame_counter.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsFilter {
    #[default]
    All,
    Category(DesktopLogCategory),
}

impl LogsFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Category(category) => category.label(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LogsState {
    pub filter: LogsFilter,
}

#[derive(Debug, Clone, Default)]
pub struct OnboardingState {
    pub first_launch_redirect_done: bool,
}

#[derive(Debug, Clone)]
pub struct ShellState {
    pub activity: Activity,
    pub chat: ChatState,
    pub peers: PeersState,
    pub operator: OperatorState,
    pub logs: LogsState,
    pub onboarding: OnboardingState,
    pub identity: IdentityState,
    pub status: StatusBarState,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            activity: Activity::Chat,
            chat: ChatState {
                transcript_stick_to_bottom: true,
                ..ChatState::default()
            },
            peers: PeersState::default(),
            operator: OperatorState::default(),
            logs: LogsState::default(),
            onboarding: OnboardingState::default(),
            identity: IdentityState {
                initials: "D1",
                label: "FIELD PRINCIPAL".to_string(),
                did_short: "did:defra:9v6q..p0ra".to_string(),
            },
            status: StatusBarState {
                peered_now: 2,
                peered_target: 3,
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
