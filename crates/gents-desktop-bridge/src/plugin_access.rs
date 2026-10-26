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
    pub subject: ApprovalSubjectView,
    pub answers: Vec<approval::Answer>,
    pub session_id: Option<String>,
    #[ts(type = "number")]
    pub expires_at_ms: u64,
}

#[derive(Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ApprovalSubjectView {
    Filesystem {
        path: std::path::PathBuf,
        folder: std::path::PathBuf,
        #[serde(rename = "isDir")]
        is_dir: bool,
        access: BindAccess,
    },
    Command {
        argv: Vec<String>,
        cwd: std::path::PathBuf,
        root: std::path::PathBuf,
        #[serde(rename = "executionMode")]
        execution_mode: gents::toolset::CommandExecutionMode,
        #[serde(rename = "networkMode")]
        network_mode: gents::toolset::CommandNetworkMode,
    },
}

impl From<&approval::Request> for PluginApprovalView {
    fn from(request: &approval::Request) -> Self {
        let subject = if let Some(file) = request.filesystem() {
            ApprovalSubjectView::Filesystem {
                path: file.path.to_owned(),
                folder: file.folder.to_owned(),
                is_dir: file.is_dir,
                access: file.access,
            }
        } else {
            let command = request
                .command()
                .expect("a non-filesystem approval has a command");
            ApprovalSubjectView::Command {
                argv: command.argv.to_vec(),
                cwd: command.cwd.to_owned(),
                root: command.root.to_owned(),
                execution_mode: command.policy.mode,
                network_mode: command.policy.network_mode,
            }
        };
        Self {
            id: request.id.clone(),
            prompt: request.prompt(),
            subject,
            answers: request.answers(),
            session_id: request.session_id.clone(),
            expires_at_ms: request.expires_at_ms,
        }
    }
}

pub(crate) fn answer(value: &str) -> Result<approval::Answer, BridgeError> {
    match value {
        "once" => Ok(approval::Answer::Once),
        "invocation" => Ok(approval::Answer::AlwaysInvocation),
        "file" => Ok(approval::Answer::AlwaysPath),
        "always" => Ok(approval::Answer::AlwaysFolder),
        "deny" => Ok(approval::Answer::Deny),
        other => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("{other:?} is not an answer; use once, invocation, file, always or deny"),
        )),
    }
}
