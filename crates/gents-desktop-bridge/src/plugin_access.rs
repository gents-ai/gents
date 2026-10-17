use gents::pack::BindAccess;
use gents::plugin::{allowed, approval};
use serde::Serialize;
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};

#[derive(Debug, Serialize, TS)]
pub struct AllowedFoldersView {
    pub dirs: Vec<allowed::AllowedDir>,
}

#[derive(Debug, Serialize, TS)]
pub struct PluginApprovalsView {
    pub requests: Vec<PluginApprovalView>,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginApprovalView {
    pub id: String,
    pub prompt: String,
    pub folder: std::path::PathBuf,
    pub path: String,
    pub is_dir: bool,
    pub session_id: Option<String>,
    pub access: BindAccess,
    #[ts(type = "number")]
    pub expires_at_ms: u64,
}

impl From<&approval::Request> for PluginApprovalView {
    fn from(request: &approval::Request) -> Self {
        Self {
            id: request.id.clone(),
            prompt: request.prompt(),
            folder: request.folder.clone(),
            path: request.path.clone(),
            is_dir: request.is_dir,
            session_id: request.session_id.clone(),
            access: request.access,
            expires_at_ms: request.expires_at_ms,
        }
    }
}

pub(crate) fn answer(value: &str) -> Result<approval::Answer, BridgeError> {
    match value {
        "once" => Ok(approval::Answer::Once),
        "file" => Ok(approval::Answer::AlwaysPath),
        "always" => Ok(approval::Answer::AlwaysFolder),
        "deny" => Ok(approval::Answer::Deny),
        other => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("{other:?} is not an answer; use once, file, always or deny"),
        )),
    }
}
