use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{allowed, now_ms, workspace, Answer, WAIT};
use crate::pack::BindAccess;
use gents_loop::tool_policy::workspace_permissions::{self as policy, ApprovalKind};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WireRequest", into = "WireRequest")]
pub struct Request {
    pub id: String,
    pub session_id: Option<String>,
    pub created_at_ms: u64,
    /// The waiting call publishes its actual deadline, including shorter waits.
    pub expires_at_ms: u64,
    subject: Subject,
    command_grant: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Subject {
    Plugin {
        plugin: String,
        path: PathBuf,
        is_dir: bool,
        access: BindAccess,
    },
    Workspace(Box<workspace::Binding>),
}

pub struct FilesystemApproval<'a> {
    pub label: &'a str,
    pub path: &'a Path,
    pub folder: &'a Path,
    pub is_dir: bool,
    pub access: BindAccess,
}

pub struct CommandApproval<'a> {
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub root: &'a Path,
    pub policy: &'a crate::toolset::CommandExecutionPolicy,
}

impl Request {
    pub fn new(
        plugin: &str,
        resolved: &allowed::Resolved,
        access: BindAccess,
        session_id: Option<String>,
    ) -> Self {
        Self::with_subject(
            Subject::Plugin {
                plugin: plugin.to_owned(),
                path: resolved.target.clone(),
                is_dir: resolved.is_dir,
                access,
            },
            session_id,
        )
    }

    pub(crate) fn for_binding(binding: workspace::Binding, session_id: Option<String>) -> Self {
        let supported = matches!(binding.action, workspace::BindingAction::Command { .. });
        let mut request = Self::with_subject(Subject::Workspace(Box::new(binding)), session_id);
        request.command_grant = supported;
        request
    }

    fn with_subject(subject: Subject, session_id: Option<String>) -> Self {
        let created_at_ms = now_ms();
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            session_id,
            created_at_ms,
            expires_at_ms: created_at_ms.saturating_add(WAIT.as_millis() as u64),
            subject,
            command_grant: false,
        }
    }

    pub(crate) fn binding(&self) -> Option<&workspace::Binding> {
        match &self.subject {
            Subject::Workspace(binding) => Some(binding),
            _ => None,
        }
    }

    pub fn filesystem(&self) -> Option<FilesystemApproval<'_>> {
        match &self.subject {
            Subject::Plugin {
                plugin,
                path,
                is_dir,
                access,
            } => Some(FilesystemApproval::new(plugin, path, *is_dir, *access)),
            Subject::Workspace(binding) => binding.action.filesystem(),
        }
    }

    pub fn command(&self) -> Option<CommandApproval<'_>> {
        match self.binding().map(|binding| &binding.action) {
            Some(workspace::BindingAction::Command {
                argv,
                cwd,
                root,
                policy,
                ..
            }) => Some(CommandApproval {
                argv,
                cwd,
                root,
                policy,
            }),
            _ => None,
        }
    }

    pub(super) fn kind(&self) -> ApprovalKind {
        match self.binding().map(|binding| &binding.action) {
            Some(workspace::BindingAction::Command { .. }) => ApprovalKind::Command,
            Some(workspace::BindingAction::FileMutation { .. }) => ApprovalKind::FileMutation,
            _ => ApprovalKind::Filesystem,
        }
    }

    pub fn allows_answer(&self, answer: Answer) -> bool {
        if answer == Answer::AlwaysInvocation {
            policy::can_remember_command(self.kind(), self.command_grant)
        } else {
            policy::answer_allowed(self.kind(), answer.remembered())
        }
    }

    pub fn answers(&self) -> Vec<Answer> {
        [
            Answer::Once,
            Answer::AlwaysPath,
            Answer::AlwaysFolder,
            Answer::AlwaysInvocation,
            Answer::Deny,
        ]
        .into_iter()
        .filter(|answer| self.allows_answer(*answer))
        .filter(|answer| {
            *answer != Answer::AlwaysPath || !self.filesystem().is_some_and(|file| file.is_dir)
        })
        .collect()
    }

    pub fn prompt(&self) -> String {
        if let Some(workspace::BindingAction::FileMutation { tool, path, .. }) =
            self.binding().map(|binding| &binding.action)
        {
            let verb = match tool.as_str() {
                "edit_file" => "Edit",
                "lsp_edit" => "Apply language-server changes to",
                _ => "Write",
            };
            return format!("{verb} {}?", path.display());
        }
        if let Some(command) = self.command() {
            return format!(
                "Run {} in {}?",
                crate::toolset::shell_join(command.argv),
                command.cwd.display()
            );
        }
        let file = self
            .filesystem()
            .expect("a non-command approval has a filesystem target");
        let verb = match file.access {
            BindAccess::Read => "read",
            BindAccess::ReadWrite => "read and write",
        };
        format!("Allow {} to {verb} {}?", file.label, file.path.display())
    }
}

impl<'a> FilesystemApproval<'a> {
    pub(super) fn new(label: &'a str, path: &'a Path, is_dir: bool, access: BindAccess) -> Self {
        let folder = if is_dir {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        Self {
            label,
            path,
            folder,
            is_dir,
            access,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    command_grant: bool,
    #[serde(default)]
    session_id: Option<String>,
    created_at_ms: u64,
    expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<workspace::Binding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_dir: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access: Option<BindAccess>,
}

impl TryFrom<WireRequest> for Request {
    type Error = anyhow::Error;
    fn try_from(wire: WireRequest) -> Result<Self> {
        let subject = if let Some(binding) = wire.binding {
            anyhow::ensure!(
                wire.plugin.is_none()
                    && wire.path.is_none()
                    && wire.folder.is_none()
                    && wire.is_dir.is_none()
                    && wire.access.is_none(),
                "a workspace approval cannot contain a second filesystem target"
            );
            Subject::Workspace(Box::new(binding))
        } else {
            let plugin = wire.plugin.context("the approval question has no plugin")?;
            let path = wire.path.context("the approval question has no path")?;
            let is_dir = wire.is_dir.unwrap_or(false);
            let access = wire
                .access
                .context("the approval question has no access level")?;
            let file = FilesystemApproval::new(&plugin, &path, is_dir, access);
            anyhow::ensure!(
                wire.folder.as_deref() == Some(file.folder),
                "the approval question's folder does not match its target"
            );
            Subject::Plugin {
                plugin,
                path,
                is_dir,
                access,
            }
        };
        anyhow::ensure!(
            !wire.command_grant
                || matches!(&subject, Subject::Workspace(binding) if matches!(binding.action, workspace::BindingAction::Command { .. })),
            "Remembered invocation support requires a bound command."
        );
        Ok(Self {
            command_grant: wire.command_grant,
            id: wire.id,
            session_id: wire.session_id,
            created_at_ms: wire.created_at_ms,
            expires_at_ms: wire.expires_at_ms,
            subject,
        })
    }
}

impl From<Request> for WireRequest {
    fn from(request: Request) -> Self {
        let mut wire = Self {
            command_grant: request.command_grant,
            id: request.id,
            session_id: request.session_id,
            created_at_ms: request.created_at_ms,
            expires_at_ms: request.expires_at_ms,
            binding: None,
            plugin: None,
            path: None,
            folder: None,
            is_dir: None,
            access: None,
        };
        match request.subject {
            Subject::Workspace(binding) => wire.binding = Some(*binding),
            Subject::Plugin {
                plugin,
                path,
                is_dir,
                access,
            } => {
                wire.folder = Some(
                    FilesystemApproval::new(&plugin, &path, is_dir, access)
                        .folder
                        .to_owned(),
                );
                wire.plugin = Some(plugin);
                wire.path = Some(path);
                wire.is_dir = Some(is_dir);
                wire.access = Some(access);
            }
        }
        wire
    }
}
