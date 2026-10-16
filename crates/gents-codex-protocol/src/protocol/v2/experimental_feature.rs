use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalFeatureListParams {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub thread_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentalFeatureStage {
    Beta,
    UnderDevelopment,
    Stable,
    Deprecated,
    Removed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalFeature {
    pub name: String,
    pub stage: ExperimentalFeatureStage,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub announcement: Option<String>,
    pub enabled: bool,
    pub default_enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalFeatureListResponse {
    pub data: Vec<ExperimentalFeature>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalFeatureEnablementSetParams {
    pub enablement: BTreeMap<String, bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalFeatureEnablementSetResponse {
    pub enablement: BTreeMap<String, bool>,
}
