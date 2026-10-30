use rapidhash::RapidHashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::super::context::ToolError;
#[cfg(target_os = "macos")]
use super::command_credentials;
use super::command_env::build_shell_env;
use super::command_temp;
use super::command_validation::policy_denial;
#[cfg(target_os = "macos")]
use super::SANDBOX_EXEC;
use super::{CommandExecutionMode, CommandExecutionPolicy, CommandNetworkMode, WorkspaceAuthority};
use crate::tool_call_lifecycle::FailureClass;
use crate::toolset::DenialReason;

#[cfg(test)]
pub(crate) fn select_sandbox_for_policy(
    mode: CommandExecutionMode,
    workspace_write_sandbox_enforced: bool,
) -> Result<&'static str> {
    match mode {
        CommandExecutionMode::ReadOnly => Ok("policy_read_only"),
        CommandExecutionMode::Unrestricted => Ok("unsandboxed_unrestricted"),
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite
            if workspace_write_sandbox_enforced =>
        {
            Ok("macos_seatbelt")
        }
        CommandExecutionMode::ArtifactWrite => {
            anyhow::bail!("artifact_write requires macOS sandbox-exec enforcement")
        }
        CommandExecutionMode::WorkspaceWrite => {
            if cfg!(target_os = "macos") {
                anyhow::bail!(
                    "macOS sandbox-exec is required for workspace_write bash but was not found"
                )
            } else {
                anyhow::bail!(
                    "workspace_write bash requires macOS seatbelt sandbox enforcement on this build"
                )
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn workspace_write_sandbox_enforced() -> bool {
    Path::new(SANDBOX_EXEC).exists()
}

#[cfg(not(target_os = "macos"))]
pub fn workspace_write_sandbox_enforced() -> bool {
    false
}

pub(super) fn artifact_policy_denial(message: impl Into<String>) -> ToolError {
    ToolError::reported_failure(FailureClass::PolicyDenied, message.into())
}

pub(super) struct CommandLaunch {
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) environment: RapidHashMap<String, String>,
    pub(super) sandbox: &'static str,
    pub(super) writable_root: Option<PathBuf>,
    pub(super) protection: Option<crate::plugin::allowed::CommandProtection>,
    #[cfg(target_os = "macos")]
    pub(super) credentials: Option<command_credentials::CredentialProtection>,
}

impl CommandLaunch {
    pub(super) async fn revalidate(&self) -> Result<()> {
        if let Some(protection) = &self.protection {
            protection.revalidate()?;
        }
        #[cfg(target_os = "macos")]
        if let Some(credentials) = &self.credentials {
            credentials.revalidate().await?;
        }
        Ok(())
    }
}

pub(super) async fn prepare_command_launch(
    source_root: &Path,
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<CommandLaunch, ToolError> {
    let mut environment = build_shell_env();
    let artifact = if policy.mode == CommandExecutionMode::ArtifactWrite {
        let scope = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .ok_or_else(|| {
                artifact_policy_denial("artifact_write requires a live sealed workspace request")
            })?;
        if scope.workspace_authority != Some(WorkspaceAuthority::ReadOnly) {
            return Err(artifact_policy_denial(
                "artifact_write requires a ReadOnly workspace binding",
            ));
        }
        let grant = scope.workspace_artifact.ok_or_else(|| {
            artifact_policy_denial("artifact_write requires a runtime-owned artifact grant")
        })?;
        grant.validate_for_launch().await.map_err(|error| {
            artifact_policy_denial(format!("artifact execution grant rejected: {error:#}"))
        })?;
        let source_root = std::fs::canonicalize(source_root).map_err(|error| {
            artifact_policy_denial(format!("artifact source unavailable: {error}"))
        })?;
        if source_root != grant.source_root() {
            return Err(artifact_policy_denial(
                "artifact grant source does not match the command tool root",
            ));
        }
        environment.insert(
            "CARGO_TARGET_DIR".into(),
            grant.root().join("target").to_string_lossy().into_owned(),
        );
        for name in ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"] {
            environment.insert(name.into(), String::new());
        }
        for name in ["TMPDIR", "TMP", "TEMP"] {
            environment.insert(
                name.into(),
                grant.root().join("tmp").to_string_lossy().into_owned(),
            );
        }
        Some(grant)
    } else {
        None
    };
    let workspace_root =
        crate::plugin::approval::workspace::command_writable_root(source_root, policy).map_err(
            |error| ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}")),
        )?;
    let writable_root = artifact
        .as_ref()
        .map(|grant| grant.root())
        .unwrap_or(&workspace_root);
    if policy.mode == CommandExecutionMode::WorkspaceWrite {
        command_temp::prepare(writable_root, &mut environment).map_err(command_guard_error)?;
    }
    let protection = crate::plugin::approval::workspace::command_guard_home()
        .map_err(command_guard_error)?
        .map(|home| {
            crate::plugin::allowed::CommandProtection::capture(
                &home,
                workspace_write_sandbox_enforced(),
            )
        })
        .transpose()
        .map_err(command_guard_error)?;
    let (program, argv, sandbox) = match &protection {
        Some(protection) => {
            protected_command_for_policy(writable_root, command, args, policy, protection)
                .map_err(command_guard_error)?
        }
        None => sandboxed_command_for_policy(writable_root, command, args, policy)?,
    };
    #[cfg(target_os = "macos")]
    let (argv, credentials) = if policy.mode.protects_credentials() {
        let credentials = command_credentials::capture()
            .await
            .map_err(command_guard_error)?;
        let mut argv = argv;
        credentials
            .protect_launch(&mut argv)
            .map_err(command_guard_error)?;
        (argv, Some(credentials))
    } else {
        (argv, None)
    };
    Ok(CommandLaunch {
        program,
        args: argv,
        environment,
        sandbox,
        writable_root: (policy.mode == CommandExecutionMode::WorkspaceWrite)
            .then(|| writable_root.to_owned()),
        protection,
        #[cfg(target_os = "macos")]
        credentials,
    })
}

pub(super) fn command_guard_error(error: anyhow::Error) -> ToolError {
    ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
}

pub(crate) async fn prepare_declared_cli_command(
    cwd: &Path,
    command: &str,
    args: &[String],
    allowed_prefixes: &[Vec<String>],
) -> std::result::Result<Vec<String>, ToolError> {
    let policy = CommandExecutionPolicy::write_capable()
        .with_mode(CommandExecutionMode::Unrestricted)
        .with_allowed_argv_prefixes(
            allowed_prefixes
                .iter()
                .map(|prefix| {
                    std::iter::once(command.to_owned())
                        .chain(prefix.iter().cloned())
                        .collect()
                })
                .collect(),
        );
    let protection = crate::plugin::approval::workspace::command_guard_home()
        .map_err(command_guard_error)?
        .map(|home| {
            crate::plugin::allowed::CommandProtection::capture(
                &home,
                workspace_write_sandbox_enforced(),
            )
        })
        .transpose()
        .map_err(command_guard_error)?;
    let argv: Vec<_> = std::iter::once(command.to_owned())
        .chain(args.iter().cloned())
        .collect();
    crate::plugin::approval::workspace::authorize_command(&argv, cwd, cwd, None, &policy)
        .await
        .map_err(command_guard_error)?;
    match protection {
        Some(protection) => {
            protection.revalidate().map_err(command_guard_error)?;
            let (program, args, _) =
                protected_command_for_policy(cwd, command, args, &policy, &protection)
                    .map_err(command_guard_error)?;
            Ok(std::iter::once(program).chain(args).collect())
        }
        None => Ok(argv),
    }
}

#[cfg(target_os = "macos")]
fn protected_command_for_policy(
    root: &Path,
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
    protection: &crate::plugin::allowed::CommandProtection,
) -> Result<(String, Vec<String>, &'static str)> {
    let bounded = matches!(
        policy.mode,
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite
    );
    let mut profile = if bounded {
        macos_workspace_write_policy(policy.effective_network_mode())
    } else {
        "(version 1)\n(allow default)\n".to_owned()
    };
    if policy.mode == CommandExecutionMode::ArtifactWrite {
        profile.push_str("\n(deny file-link)\n");
    }
    let mut parameters = vec![format!(
        "-DWRITABLE_ROOT={}",
        root.to_str().context("command root is not valid UTF-8")?
    )];
    for (index, path) in protection.paths().iter().enumerate() {
        profile.push_str(&format!(
            "\n(deny file-read* file-write* (subpath (param \"OPERATOR_{index}\")))\n"
        ));
        parameters.push(format!(
            "-DOPERATOR_{index}={}",
            path.to_str()
                .context("permission path is not valid UTF-8")?
        ));
    }
    for (index, path) in protection.ancestors().iter().enumerate() {
        profile.push_str(&format!(
            "\n(deny file-write-unlink (literal (param \"OPERATOR_PARENT_{index}\")))\n"
        ));
        parameters.push(format!(
            "-DOPERATOR_PARENT_{index}={}",
            path.to_str()
                .context("permission parent is not valid UTF-8")?
        ));
    }
    let argv = std::iter::once("-p".to_owned())
        .chain(std::iter::once(profile))
        .chain(parameters)
        .chain(["--".to_owned(), command.to_owned()])
        .chain(args.iter().cloned())
        .collect();
    Ok((
        SANDBOX_EXEC.to_owned(),
        argv,
        if bounded {
            "macos_seatbelt"
        } else {
            "macos_operator_state_guard"
        },
    ))
}

#[cfg(not(target_os = "macos"))]
fn protected_command_for_policy(
    _root: &Path,
    _command: &str,
    _args: &[String],
    _policy: &CommandExecutionPolicy,
    _protection: &crate::plugin::allowed::CommandProtection,
) -> Result<(String, Vec<String>, &'static str)> {
    anyhow::bail!("Command isolation for runtime permissions is unavailable on this platform.")
}

fn sandboxed_command_for_policy(
    root: &Path,
    command_name: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(String, Vec<String>, &'static str), ToolError> {
    let sandbox = select_sandbox_for_execution(policy)?;
    match policy.mode {
        CommandExecutionMode::ReadOnly => Ok((command_name.to_string(), args.to_vec(), sandbox)),
        CommandExecutionMode::Unrestricted => {
            Ok((command_name.to_string(), args.to_vec(), sandbox))
        }
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite => {
            sandboxed_workspace_write_command(
                root,
                command_name,
                args,
                policy.effective_network_mode(),
                sandbox,
                policy.mode == CommandExecutionMode::ArtifactWrite,
            )
            .map_err(Into::into)
        }
    }
}

fn select_sandbox_for_execution(
    policy: &CommandExecutionPolicy,
) -> std::result::Result<&'static str, ToolError> {
    match policy.mode {
        CommandExecutionMode::ReadOnly => Ok("policy_read_only"),
        CommandExecutionMode::Unrestricted => Ok("unsandboxed_unrestricted"),
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite
            if workspace_write_sandbox_enforced() =>
        {
            Ok("macos_seatbelt")
        }
        CommandExecutionMode::ArtifactWrite => Err(policy_denial(
            policy,
            DenialReason::ArtifactWriteSandboxUnavailable,
        )),
        CommandExecutionMode::WorkspaceWrite => Err(policy_denial(
            policy,
            DenialReason::WorkspaceWriteSandboxUnavailable,
        )),
    }
}

#[cfg(target_os = "macos")]
fn sandboxed_workspace_write_command(
    root: &Path,
    command_name: &str,
    args: &[String],
    network_mode: CommandNetworkMode,
    sandbox: &'static str,
    artifact_only: bool,
) -> Result<(String, Vec<String>, &'static str)> {
    let mut policy = macos_workspace_write_policy(network_mode);
    if artifact_only {
        policy.push_str("\n(deny file-link)\n");
    }
    let mut sandbox_args = vec![
        "-p".to_string(),
        policy,
        format!("-DWRITABLE_ROOT={}", root.display()),
        "--".to_string(),
        command_name.to_string(),
    ];
    sandbox_args.extend(args.iter().cloned());
    Ok((SANDBOX_EXEC.to_string(), sandbox_args, sandbox))
}

#[cfg(not(target_os = "macos"))]
fn sandboxed_workspace_write_command(
    _root: &Path,
    _command_name: &str,
    _args: &[String],
    _network_mode: CommandNetworkMode,
    _sandbox: &'static str,
    _artifact_only: bool,
) -> Result<(String, Vec<String>, &'static str)> {
    anyhow::bail!("workspace_write bash requires macOS seatbelt sandbox enforcement on this build")
}

#[cfg(target_os = "macos")]
/// Chromium registers its parent/child rendezvous before opening a page. These
/// bootstrap names include its bundle ID and PID; permit only the rendezvous
/// namespaces, not unrelated global services or additional filesystem access.
/// See [Chromium's rendezvous owner](https://chromium.googlesource.com/chromium/src/+/main/base/apple/mach_port_rendezvous_mac.cc).
pub(super) fn macos_workspace_write_policy(network_mode: CommandNetworkMode) -> String {
    let network_policy = if network_mode.allows_network() {
        "(allow network-outbound)\n(allow network-inbound)\n"
    } else {
        ""
    };
    format!(
        r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)
(allow mach-lookup)
(allow mach-register
    (global-name-prefix "org.chromium.Chromium.MachPortRendezvousServer.")
    (global-name-prefix "com.google.chrome.for.testing.MachPortRendezvousServer."))
(allow ipc-posix-sem)
(allow ipc-posix-shm-read*)
(allow ipc-posix-shm-write*)
(allow file-read*)
(allow file-write-data (literal "/dev/null"))
(allow file-write* (subpath (param "WRITABLE_ROOT")))
{network_policy}"#
    )
}
