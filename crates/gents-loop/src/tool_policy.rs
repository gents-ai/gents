//! Pure command-policy vocabulary shared between the loop's tool-dispatch
//! outcome (`ToolOutcome::Failed { denial, .. }`) and `gents`'s native
//! command-execution enforcement (`toolset::shared::command`).
//!
//! Everything here is data and pure classification: no process spawn, no
//! filesystem, no DefraDB. The enforcement itself (building a
//! `CommandExecutionPolicy`, running `sandbox-exec`, spawning `managed_exec`)
//! stays in `gents`, which re-exports these types so its own call sites are
//! unchanged.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecutionMode {
    ReadOnly,
    WorkspaceWrite,
    ArtifactWrite,
    Unrestricted,
}

impl CommandExecutionMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "read_only" | "ReadOnly" => Ok(Self::ReadOnly),
            "workspace_write" | "WorkspaceWrite" | "managed_write" | "ManagedWrite" => {
                Ok(Self::WorkspaceWrite)
            }
            "artifact_write" | "ArtifactWrite" => Ok(Self::ArtifactWrite),
            "unrestricted" | "Unrestricted" => Ok(Self::Unrestricted),
            other => bail!("unknown command execution policy mode {other}"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::ArtifactWrite => "artifact_write",
            Self::Unrestricted => "unrestricted",
        }
    }

    /// Intersect effects: source writes and private artifact writes are incomparable.
    pub fn meet(self, other: Self) -> Self {
        if self == other {
            return self;
        }
        match (self, other) {
            (Self::Unrestricted, mode) | (mode, Self::Unrestricted) => mode,
            _ => Self::ReadOnly,
        }
    }
}

/// Request `workspace_authority`. ReadWrite meets command mode to WorkspaceWrite,
/// never Unrestricted. Integrate is inspect-only (no bash writes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAuthority {
    ReadOnly,
    ReadWrite,
    Integrate,
}

impl WorkspaceAuthority {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "readOnly" | "ReadOnly" | "read_only" => Ok(Self::ReadOnly),
            "readWrite" | "ReadWrite" | "read_write" => Ok(Self::ReadWrite),
            "integrate" | "Integrate" => Ok(Self::Integrate),
            other => bail!("unknown workspace authority {other}"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "readOnly",
            Self::ReadWrite => "readWrite",
            Self::Integrate => "integrate",
        }
    }

    pub fn command_mode(self) -> CommandExecutionMode {
        match self {
            Self::ReadOnly | Self::Integrate => CommandExecutionMode::ReadOnly,
            Self::ReadWrite => CommandExecutionMode::WorkspaceWrite,
        }
    }

    pub fn allows_file_writes(self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    /// Greatest lower bound. Child spawn cannot outrank a bound parent.
    pub fn infimum(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Integrate => 1,
            Self::ReadWrite => 2,
        }
    }

    pub fn bindable_lifecycle_state(self, state: &str) -> bool {
        matches!(
            (self, normalize_workspace_lifecycle_state(state)),
            (Self::ReadWrite, Some("ready"))
                | (Self::ReadOnly, Some("ready" | "sealed"))
                | (Self::Integrate, Some("sealed"))
        )
    }
}

pub fn normalize_workspace_lifecycle_state(value: &str) -> Option<&'static str> {
    match value.trim() {
        "provisioning" | "Provisioning" => Some("provisioning"),
        "ready" | "Ready" => Some("ready"),
        "provisionFailed" | "provision_failed" | "ProvisionFailed" => Some("provisionFailed"),
        "sealed" | "Sealed" => Some("sealed"),
        "cleaning" | "Cleaning" => Some("cleaning"),
        "cleaned" | "Cleaned" => Some("cleaned"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandNetworkMode {
    Inherit,
    Disabled,
    Enabled,
}

impl CommandNetworkMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "inherit" | "Inherit" => Ok(Self::Inherit),
            "disabled" | "Disabled" | "off" | "Off" => Ok(Self::Disabled),
            "enabled" | "Enabled" | "on" | "On" => Ok(Self::Enabled),
            other => bail!("unknown command network mode {other}"),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
        }
    }

    pub fn allows_network(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// More restrictive mode wins: Disabled < Inherit < Enabled.
    pub fn meet(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Inherit => 1,
            Self::Enabled => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenialReason {
    ForbiddenPrefix { matched: Vec<String> },
    AllowedPrefixRequired { argv: Vec<String> },
    ReadOnlyCommandNotAllowlisted { command: String },
    ReadOnlyArgumentNotAllowed { command: String, argument: String },
    ReadOnlySubcommandRequired { command: String },
    ReadOnlySubcommandNotAllowlisted { command: String, subcommand: String },
    ReadOnlyUrlRequired { command: String },
    DisabledNetworkUnenforceable,
    DisabledNetworkCommand { command: String },
    WorkspaceWriteSandboxUnavailable,
    ArtifactWriteSandboxUnavailable,
    WorkspaceExecutable,
    GitMetadataWriteDenied { command: String, subcommand: String },
}

impl DenialReason {
    pub fn to_contract(&self) -> &'static str {
        match self {
            Self::ForbiddenPrefix { .. } => "forbiddenPrefix",
            Self::AllowedPrefixRequired { .. } => "allowedPrefixRequired",
            Self::ReadOnlyCommandNotAllowlisted { .. } => "readOnlyCommandNotAllowlisted",
            Self::ReadOnlyArgumentNotAllowed { .. } => "readOnlyArgumentNotAllowed",
            Self::ReadOnlySubcommandRequired { .. } => "readOnlySubcommandRequired",
            Self::ReadOnlySubcommandNotAllowlisted { .. } => "readOnlySubcommandNotAllowlisted",
            Self::ReadOnlyUrlRequired { .. } => "readOnlyUrlRequired",
            Self::DisabledNetworkUnenforceable => "disabledNetworkUnenforceable",
            Self::DisabledNetworkCommand { .. } => "disabledNetworkCommand",
            Self::WorkspaceWriteSandboxUnavailable => "workspaceWriteSandboxUnavailable",
            Self::ArtifactWriteSandboxUnavailable => "artifactWriteSandboxUnavailable",
            Self::WorkspaceExecutable => "workspaceExecutable",
            Self::GitMetadataWriteDenied { .. } => "gitMetadataWriteDenied",
        }
    }

    pub fn matched_prefix(&self) -> Option<&[String]> {
        match self {
            Self::ForbiddenPrefix { matched } => Some(matched),
            _ => None,
        }
    }

    pub fn denied_argv(&self) -> Option<&[String]> {
        match self {
            Self::AllowedPrefixRequired { argv } => Some(argv),
            _ => None,
        }
    }

    pub fn denied_command(&self) -> Option<&str> {
        match self {
            Self::ReadOnlyCommandNotAllowlisted { command }
            | Self::ReadOnlyArgumentNotAllowed { command, .. }
            | Self::ReadOnlySubcommandRequired { command }
            | Self::ReadOnlySubcommandNotAllowlisted { command, .. }
            | Self::ReadOnlyUrlRequired { command }
            | Self::DisabledNetworkCommand { command }
            | Self::GitMetadataWriteDenied { command, .. } => Some(command),
            _ => None,
        }
    }

    pub fn denied_argument(&self) -> Option<&str> {
        match self {
            Self::ReadOnlyArgumentNotAllowed { argument, .. } => Some(argument),
            _ => None,
        }
    }

    pub fn denied_subcommand(&self) -> Option<&str> {
        match self {
            Self::ReadOnlySubcommandNotAllowlisted { subcommand, .. }
            | Self::GitMetadataWriteDenied { subcommand, .. } => Some(subcommand),
            _ => None,
        }
    }

    pub fn diagnostic(&self) -> String {
        match self {
            Self::ForbiddenPrefix { matched } => format!(
                "command is forbidden by command execution policy prefix: {}",
                shell_join_display(matched)
            ),
            Self::AllowedPrefixRequired { argv } => format!(
                "command is not allowed by command execution policy prefixes: {}",
                shell_join_display(argv)
            ),
            Self::ReadOnlyCommandNotAllowlisted { command } => {
                format!("command is not allowed by the read-only bash tool: {command}")
            }
            Self::ReadOnlyArgumentNotAllowed { command, argument } => {
                match (command.as_str(), argument.as_str()) {
                    ("sed", "-i" | "--in-place") => "sed in-place edits are not allowed".into(),
                    ("sed", arg) if arg.starts_with("-i") || arg.starts_with("--in-place=") => {
                        "sed in-place edits are not allowed".into()
                    }
                    ("find", _) => {
                        "find arguments that can write or execute are not allowed".into()
                    }
                    ("sudo", arg) if arg.ends_with("launchctl") => {
                        "sudo launchctl must use the absolute /bin/launchctl path".into()
                    }
                    ("git", arg)
                        if arg == "-C"
                            || arg == "-c"
                            || arg.starts_with("-C")
                            || arg.starts_with("-c")
                            || arg.starts_with("--config-env")
                            || arg.starts_with("--exec-path")
                            || arg.starts_with("--git-dir")
                            || arg.starts_with("--namespace")
                            || arg.starts_with("--super-prefix")
                            || arg.starts_with("--work-tree") =>
                    {
                        "git global options that redirect config or helper lookup are not allowed"
                            .into()
                    }
                    ("git", arg) if arg == "-D" || arg.starts_with("--format=") => {
                        format!("git branch argument is not read-only: {argument}")
                    }
                    ("git", _) => {
                        format!(
                            "git argument is not allowed by the read-only bash tool: {argument}"
                        )
                    }
                    ("rg", _) => {
                        format!("rg argument is not allowed by the read-only bash tool: {argument}")
                    }
                    ("curl", _) => {
                        format!(
                            "curl argument is not allowed by the read-only bash tool: {argument}"
                        )
                    }
                    _ => format!(
                        "{command} argument is not allowed by the read-only bash tool: {argument}"
                    ),
                }
            }
            Self::ReadOnlySubcommandRequired { command } => match command.as_str() {
                "sudo" => "sudo requires an approved command".into(),
                _ => format!("{command} requires a read-only subcommand"),
            },
            Self::ReadOnlySubcommandNotAllowlisted {
                command,
                subcommand,
            } => match command.as_str() {
                "sudo" => {
                    format!("sudo command is not allowed by the read-only bash tool: {subcommand}")
                }
                _ => format!(
                    "{command} subcommand is not allowed by the read-only bash tool: {subcommand}"
                ),
            },
            Self::ReadOnlyUrlRequired { command } => {
                format!("{command} requires an http:// or https:// URL in the read-only bash tool")
            }
            Self::DisabledNetworkUnenforceable => {
                "command_network_mode=disabled cannot be enforced for unrestricted bash".into()
            }
            Self::DisabledNetworkCommand { command } => match command.as_str() {
                "tailscale" => {
                    "tailscale network probes are not allowed when command_network_mode=disabled"
                        .into()
                }
                _ => format!("{command} is not allowed when command_network_mode=disabled"),
            },
            Self::ArtifactWriteSandboxUnavailable => "artifact_write requires macOS Seatbelt sandbox enforcement".into(),
            Self::WorkspaceWriteSandboxUnavailable => {
                if cfg!(target_os = "macos") {
                    "macOS sandbox-exec is required for workspace_write bash but was not found"
                        .into()
                } else {
                    "workspace_write bash requires macOS seatbelt sandbox enforcement on this build"
                        .into()
                }
            }
            Self::WorkspaceExecutable => {
                "language-server executable is not admitted (workspace-local or missing)".into()
            }
            Self::GitMetadataWriteDenied {
                command,
                subcommand,
            } => format!(
                "{command} {subcommand} is denied under WorkspaceWrite+git_worktree_diff (shared Git metadata is integrator-only)"
            ),
        }
    }

    pub fn from_contract_fields(
        reason: &str,
        matched_prefix: Option<Vec<String>>,
        denied_argv: Option<Vec<String>>,
        denied_command: Option<String>,
        denied_argument: Option<String>,
        denied_subcommand: Option<String>,
    ) -> Option<Self> {
        match reason {
            "forbiddenPrefix" => Some(Self::ForbiddenPrefix {
                matched: matched_prefix?,
            }),
            "allowedPrefixRequired" => Some(Self::AllowedPrefixRequired { argv: denied_argv? }),
            "readOnlyCommandNotAllowlisted" => Some(Self::ReadOnlyCommandNotAllowlisted {
                command: denied_command?,
            }),
            "readOnlyArgumentNotAllowed" => Some(Self::ReadOnlyArgumentNotAllowed {
                command: denied_command?,
                argument: denied_argument?,
            }),
            "readOnlySubcommandRequired" => Some(Self::ReadOnlySubcommandRequired {
                command: denied_command?,
            }),
            "readOnlySubcommandNotAllowlisted" => Some(Self::ReadOnlySubcommandNotAllowlisted {
                command: denied_command?,
                subcommand: denied_subcommand?,
            }),
            "readOnlyUrlRequired" => Some(Self::ReadOnlyUrlRequired {
                command: denied_command?,
            }),
            "disabledNetworkUnenforceable" => Some(Self::DisabledNetworkUnenforceable),
            "disabledNetworkCommand" => Some(Self::DisabledNetworkCommand {
                command: denied_command?,
            }),
            "workspaceWriteSandboxUnavailable" => Some(Self::WorkspaceWriteSandboxUnavailable),
            "artifactWriteSandboxUnavailable" => Some(Self::ArtifactWriteSandboxUnavailable),
            "workspaceExecutable" => Some(Self::WorkspaceExecutable),
            "gitMetadataWriteDenied" => Some(Self::GitMetadataWriteDenied {
                command: denied_command?,
                subcommand: denied_subcommand?,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPolicyDenial {
    pub reason: DenialReason,
    pub policy_mode: String,
    pub policy_network: String,
}

impl CommandPolicyDenial {
    pub fn new(
        reason: DenialReason,
        mode: CommandExecutionMode,
        network_mode: CommandNetworkMode,
    ) -> Self {
        Self {
            reason,
            policy_mode: mode.as_str().to_string(),
            policy_network: network_mode.as_str().to_string(),
        }
    }

    pub fn to_contract(&self) -> &'static str {
        self.reason.to_contract()
    }

    pub fn diagnostic(&self) -> String {
        self.reason.diagnostic()
    }

    pub fn payload_value(&self) -> Value {
        json!({
            "ok": false,
            "failure_class": "policyDenied",
            "denial_reason": self.reason.to_contract(),
            "denied_argv": self.reason.denied_argv(),
            "denied_command": self.reason.denied_command(),
            "denied_argument": self.reason.denied_argument(),
            "denied_subcommand": self.reason.denied_subcommand(),
            "denied_prefix": self.reason.matched_prefix(),
            "policy_mode": self.policy_mode,
            "policy_network": self.policy_network,
            "message": self.diagnostic(),
        })
    }

    pub fn tool_error_payload(&self) -> String {
        serde_json::to_string(&self.payload_value()).unwrap_or_else(|_| self.diagnostic())
    }

    pub fn from_payload_value(value: &Value) -> Option<Self> {
        let reason = value.get("denial_reason")?.as_str()?;
        let matched_prefix = string_vec_field(value, "denied_prefix");
        let denied_argv = string_vec_field(value, "denied_argv");
        let denied_command = string_field(value, "denied_command");
        let denied_argument = string_field(value, "denied_argument");
        let denied_subcommand = string_field(value, "denied_subcommand");
        let reason = DenialReason::from_contract_fields(
            reason,
            matched_prefix,
            denied_argv,
            denied_command,
            denied_argument,
            denied_subcommand,
        )?;
        Some(Self {
            reason,
            policy_mode: string_field(value, "policy_mode").unwrap_or_default(),
            policy_network: string_field(value, "policy_network").unwrap_or_default(),
        })
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_vec_field(value: &Value, key: &str) -> Option<Vec<String>> {
    value.get(key).and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect()
    })
}

fn shell_join_display(argv: &[String]) -> String {
    argv.join(" ")
}
