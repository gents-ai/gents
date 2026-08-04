use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackUploadParams {
    pub classification: String,
    pub reason: Option<String>,
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_logs: bool,
    pub extra_log_files: Option<Vec<PathBuf>>,
    pub tags: Option<BTreeMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackUploadResponse {
    pub thread_id: String,
}
