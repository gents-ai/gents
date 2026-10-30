use rapidhash::RapidHashMap;
use std::path::{Path, PathBuf};

use super::context::ToolError;

pub use gents_loop::tool_policy::{
    normalize_workspace_lifecycle_state, CommandExecutionMode, CommandNetworkMode,
    WorkspaceAuthority,
};

#[path = "command_args.rs"]
mod command_args;
#[cfg(target_os = "macos")]
#[path = "command_credentials.rs"]
mod command_credentials;
#[path = "command_env.rs"]
pub(super) mod command_env;
#[path = "command_host_executable.rs"]
pub(super) mod command_host_executable;
#[path = "command_launch.rs"]
pub(super) mod command_launch;
#[path = "command_output.rs"]
pub(super) mod command_output;
#[path = "command_run.rs"]
pub(super) mod command_run;
#[path = "command_temp.rs"]
mod command_temp;
#[path = "command_validation.rs"]
pub(super) mod command_validation;

use command_host_executable::admit_host_executable;
use command_launch::{
    artifact_policy_denial, command_guard_error, prepare_command_launch,
    workspace_write_sandbox_enforced,
};
use command_validation::{policy_denial, validate_command_policy_with_resolved_executable};

#[cfg(all(test, target_os = "macos"))]
#[path = "command_protection_tests.rs"]
mod command_protection_tests;
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub(crate) fn apply_workspace_authority(
    policy: &CommandExecutionPolicy,
    authority: WorkspaceAuthority,
) -> CommandExecutionPolicy {
    let mut met = policy.clone();
    met.mode = policy.mode.meet(authority.command_mode());
    if matches!(authority, WorkspaceAuthority::ReadWrite) {
        met = met.with_git_worktree_diff();
    }
    if matches!(met.mode, CommandExecutionMode::ReadOnly) && met.read_only_allowlist().is_empty() {
        met = met.with_read_only_allowlist(
            crate::toolset::default_read_only_command_policy()
                .read_only_allowlist()
                .to_vec(),
        );
    }
    met
}

pub(crate) fn effective_command_policy(policy: &CommandExecutionPolicy) -> CommandExecutionPolicy {
    if policy.mode == CommandExecutionMode::ArtifactWrite {
        return policy.clone();
    }
    match crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
        .and_then(|scope| scope.workspace_authority)
    {
        Some(authority) => apply_workspace_authority(policy, authority),
        None => policy.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandExecutionPolicy {
    pub mode: CommandExecutionMode,
    pub allowed_argv_prefixes: Vec<Vec<String>>,
    pub forbidden_argv_prefixes: Vec<Vec<String>>,
    pub network_mode: CommandNetworkMode,
    read_only_allowlist: Vec<String>,
    deny_all_argv: bool,
    deny_git_metadata_writes: bool,
}

impl CommandExecutionPolicy {
    pub fn read_only(allowlist: Vec<String>) -> Self {
        Self {
            mode: CommandExecutionMode::ReadOnly,
            allowed_argv_prefixes: Vec::new(),
            forbidden_argv_prefixes: Vec::new(),
            network_mode: CommandNetworkMode::Inherit,
            read_only_allowlist: allowlist,
            deny_all_argv: false,
            deny_git_metadata_writes: false,
        }
    }

    pub fn write_capable() -> Self {
        Self {
            mode: if cfg!(target_os = "macos") {
                CommandExecutionMode::WorkspaceWrite
            } else {
                CommandExecutionMode::Unrestricted
            },
            allowed_argv_prefixes: Vec::new(),
            forbidden_argv_prefixes: Vec::new(),
            network_mode: CommandNetworkMode::Inherit,
            read_only_allowlist: Vec::new(),
            deny_all_argv: false,
            deny_git_metadata_writes: false,
        }
    }

    pub fn with_mode(mut self, mode: CommandExecutionMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_allowed_argv_prefixes(mut self, prefixes: Vec<Vec<String>>) -> Self {
        self.allowed_argv_prefixes = prefixes;
        self
    }

    pub fn with_forbidden_argv_prefixes(mut self, prefixes: Vec<Vec<String>>) -> Self {
        self.forbidden_argv_prefixes = prefixes;
        self
    }

    pub fn read_only_allowlist(&self) -> &[String] {
        &self.read_only_allowlist
    }

    pub fn with_read_only_allowlist(mut self, allowlist: Vec<String>) -> Self {
        self.read_only_allowlist = allowlist;
        self
    }

    pub fn with_deny_all_argv(mut self, deny_all_argv: bool) -> Self {
        self.deny_all_argv = deny_all_argv;
        self
    }

    pub fn deny_all_argv(&self) -> bool {
        self.deny_all_argv
    }

    pub fn with_network_mode(mut self, network_mode: CommandNetworkMode) -> Self {
        self.network_mode = network_mode;
        self
    }

    pub fn with_deny_git_metadata_writes(mut self, deny: bool) -> Self {
        self.deny_git_metadata_writes = deny;
        self
    }

    pub fn with_git_worktree_diff(self) -> Self {
        self.with_deny_git_metadata_writes(true)
    }

    pub fn deny_git_metadata_writes(&self) -> bool {
        self.deny_git_metadata_writes
    }

    pub fn network_enforcement_disclosure(&self) -> &'static str {
        if self.effective_network_mode() != CommandNetworkMode::Disabled {
            return "network is not disabled by this agent policy";
        }
        match self.mode {
            CommandExecutionMode::Unrestricted => {
                "fail_closed: unrestricted commands are denied because disabled network cannot be enforced"
            }
            CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite => {
                "required_at_execution: command sandbox must enforce disabled network or the command is denied"
            }
            CommandExecutionMode::ReadOnly => {
                "validated_at_execution: known network commands are denied"
            }
        }
    }

    pub fn effective_network_mode(&self) -> CommandNetworkMode {
        self.network_mode.for_execution(self.mode)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandConstraints {
    pub allowed_argv_prefixes: Vec<Vec<String>>,
    pub forbidden_argv_prefixes: Vec<Vec<String>>,
    pub network_mode: CommandNetworkMode,
    pub execution_mode: CommandExecutionMode,
    pub sandbox: CommandExecutionMode,
    pub deny_all_argv: bool,
    pub deny_git_metadata_writes: bool,
}

impl CommandConstraints {
    pub fn to_spawn_policy(&self) -> CommandExecutionPolicy {
        CommandExecutionPolicy {
            mode: self.sandbox,
            allowed_argv_prefixes: self.allowed_argv_prefixes.clone(),
            forbidden_argv_prefixes: self.forbidden_argv_prefixes.clone(),
            network_mode: self.network_mode,
            read_only_allowlist: Vec::new(),
            deny_all_argv: self.deny_all_argv,
            deny_git_metadata_writes: self.deny_git_metadata_writes,
        }
    }
}

pub fn default_lsp_network_mode() -> CommandNetworkMode {
    if workspace_write_sandbox_enforced() {
        CommandNetworkMode::Disabled
    } else {
        CommandNetworkMode::Inherit
    }
}

pub fn lsp_sandbox_for_effective(execution_mode: CommandExecutionMode) -> CommandExecutionMode {
    match execution_mode {
        CommandExecutionMode::WorkspaceWrite => CommandExecutionMode::WorkspaceWrite,
        CommandExecutionMode::ArtifactWrite => CommandExecutionMode::ArtifactWrite,
        CommandExecutionMode::Unrestricted => CommandExecutionMode::Unrestricted,
        CommandExecutionMode::ReadOnly => {
            if workspace_write_sandbox_enforced() {
                CommandExecutionMode::WorkspaceWrite
            } else {
                CommandExecutionMode::Unrestricted
            }
        }
    }
}

pub(crate) struct PreparedManagedCommand {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) environment: RapidHashMap<String, String>,
    pub(crate) sandbox: &'static str,
    pub(crate) operator_guard_home: Option<PathBuf>,
    pub(crate) credential_snapshot: Option<Vec<PathBuf>>,
}

impl std::fmt::Debug for PreparedManagedCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedManagedCommand")
            .field("program", &self.program)
            .field("sandbox", &self.sandbox)
            .field("operator_guard_home", &self.operator_guard_home)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn prepare_managed_command(
    root: &Path,
    command: &str,
    args: &[String],
    constraints: &CommandConstraints,
) -> std::result::Result<PreparedManagedCommand, ToolError> {
    let admitted = admit_host_executable(command, root)
        .map_err(|reason| policy_denial(&constraints.to_spawn_policy(), reason))?;
    let spawn_policy = constraints.to_spawn_policy();
    if spawn_policy.mode == CommandExecutionMode::ArtifactWrite
        && constraints.execution_mode != CommandExecutionMode::ArtifactWrite
    {
        return Err(artifact_policy_denial(
            "artifact sandbox cannot elevate the effective execution mode",
        ));
    }
    validate_command_policy_with_resolved_executable(
        command,
        args,
        &admitted.to_string_lossy(),
        &spawn_policy,
    )?;
    let launch =
        prepare_command_launch(root, &admitted.to_string_lossy(), args, &spawn_policy).await?;
    launch.revalidate().await.map_err(command_guard_error)?;
    Ok(PreparedManagedCommand {
        program: PathBuf::from(launch.program),
        args: launch.args,
        environment: launch.environment,
        sandbox: launch.sandbox,
        operator_guard_home: launch
            .protection
            .as_ref()
            .map(|guard| guard.home().to_owned()),
        #[cfg(target_os = "macos")]
        credential_snapshot: launch
            .credentials
            .as_ref()
            .map(command_credentials::CredentialProtection::snapshot),
        #[cfg(not(target_os = "macos"))]
        credential_snapshot: None,
    })
}

pub(crate) async fn current_managed_command_credential_snapshot(
    protects_credentials: bool,
) -> std::result::Result<Option<Vec<PathBuf>>, String> {
    if !protects_credentials {
        return Ok(None);
    }
    #[cfg(target_os = "macos")]
    {
        command_credentials::capture()
            .await
            .map(|protection| Some(protection.snapshot()))
            .map_err(|error| format!("{error:#}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

#[cfg(test)]
#[path = "command_network_disclosure_tests.rs"]
mod network_enforcement_disclosure_tests;
