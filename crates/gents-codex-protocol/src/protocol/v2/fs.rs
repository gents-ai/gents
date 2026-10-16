use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsReadFileParams {
    pub path: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsReadFileResponse {
    pub data_base64: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsWriteFileParams {
    pub path: AbsolutePathBuf,
    pub data_base64: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsWriteFileResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsCreateDirectoryParams {
    pub path: AbsolutePathBuf,
    pub recursive: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsCreateDirectoryResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsGetMetadataParams {
    pub path: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsGetMetadataResponse {
    pub is_directory: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub created_at_ms: i64,
    pub modified_at_ms: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsReadDirectoryParams {
    pub path: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsReadDirectoryEntry {
    pub file_name: String,
    pub is_directory: bool,
    pub is_file: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsReadDirectoryResponse {
    pub entries: Vec<FsReadDirectoryEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsRemoveParams {
    pub path: AbsolutePathBuf,
    pub recursive: Option<bool>,
    pub force: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsRemoveResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsCopyParams {
    pub source_path: AbsolutePathBuf,
    pub destination_path: AbsolutePathBuf,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recursive: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsCopyResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchParams {
    pub watch_id: String,
    pub path: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsWatchResponse {
    pub path: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsUnwatchParams {
    pub watch_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsUnwatchResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FsChangedNotification {
    pub watch_id: String,
    pub changed_paths: Vec<AbsolutePathBuf>,
}
use crate::core_types::AbsolutePathBuf;
