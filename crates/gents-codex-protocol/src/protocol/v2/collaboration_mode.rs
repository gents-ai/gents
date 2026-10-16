use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationModeListParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationModeMask {
    pub name: String,
    pub mode: Option<ModeKind>,
    pub model: Option<String>,
    #[serde(rename = "reasoning_effort")]
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationModeListResponse {
    pub data: Vec<CollaborationModeMask>,
}
use crate::core_types::{ModeKind, ReasoningEffort};
