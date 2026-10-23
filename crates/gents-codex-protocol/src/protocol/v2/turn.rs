use super::ApprovalsReviewer;
use super::AskForApproval;
use super::SandboxPolicy;
use super::Turn;
use rapidhash::RapidHashMap;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnEnvironmentParams {
    pub environment_id: String,
    pub cwd: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub responsesapi_client_metadata: Option<RapidHashMap<String, String>>,
    /// Omitted uses the thread sticky environments. Empty disables
    /// environment access for this turn. Non-empty selects the first
    /// environment as the current turn environment.
    pub environments: Option<Vec<TurnEnvironmentParams>>,
    pub cwd: Option<PathBuf>,
    pub runtime_workspace_roots: Option<Vec<PathBuf>>,
    pub approval_policy: Option<AskForApproval>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub sandbox_policy: Option<SandboxPolicy>,
    /// Cannot be combined with `sandboxPolicy`.
    pub permissions: Option<String>,
    pub model: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub service_tier: Option<Option<String>>,
    pub effort: Option<ReasoningEffort>,
    pub summary: Option<ReasoningSummary>,
    pub personality: Option<Personality>,
    pub output_schema: Option<JsonValue>,

    /// EXPERIMENTAL - Set a pre-set collaboration mode. Takes precedence over
    /// model, reasoning_effort, and developer instructions if set. For
    /// `collaboration_mode.settings.developer_instructions`, `null` means
    /// "use the built-in instructions for the selected mode".
    pub collaboration_mode: Option<CollaborationMode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: Turn,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub responsesapi_client_metadata: Option<RapidHashMap<String, String>>,
    /// The request fails when this does not match the currently active turn.
    pub expected_turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResponse {
    pub turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextElement {
    pub byte_range: ByteRange,
    placeholder: Option<String>,
}

impl TextElement {
    pub fn new(byte_range: ByteRange, placeholder: Option<String>) -> Self {
        Self {
            byte_range,
            placeholder,
        }
    }

    pub fn set_placeholder(&mut self, placeholder: Option<String>) {
        self.placeholder = placeholder;
    }

    pub fn placeholder(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text {
        text: String,
        #[serde(default)]
        text_elements: Vec<TextElement>,
    },
    Image {
        #[serde(default)]
        detail: Option<ImageDetail>,
        url: String,
    },
    LocalImage {
        #[serde(default)]
        detail: Option<ImageDetail>,
        path: PathBuf,
    },
    Skill {
        name: String,
        path: PathBuf,
    },
    Mention {
        name: String,
        path: String,
    },
}

impl UserInput {
    pub fn text_char_count(&self) -> usize {
        match self {
            UserInput::Text { text, .. } => text.chars().count(),
            UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Skill { .. }
            | UserInput::Mention { .. } => 0,
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartedNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: i32,
    pub cached_input_tokens: i32,
    pub output_tokens: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiffUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub diff: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnPlanUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub explanation: Option<String>,
    pub plan: Vec<TurnPlanStep>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TurnPlanStep {
    pub step: String,
    pub status: TurnPlanStepStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TurnPlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

use crate::core_types::{
    AbsolutePathBuf, CollaborationMode, ImageDetail, Personality, ReasoningEffort, ReasoningSummary,
};
