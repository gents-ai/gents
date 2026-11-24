use rapidhash::RapidHashMap;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::Result;

use super::super::context::ToolError;
use super::command_args::read_only_launch_args;
#[cfg(target_os = "macos")]
use super::command_credentials;
use super::command_env::build_shell_env;
use super::command_host_executable::admit_host_executable;
use super::command_temp;
use super::command_validation::{executable_name_lookup_key, policy_denial};
use super::{CommandExecutionMode, CommandExecutionPolicy, CommandNetworkMode, WorkspaceAuthority};
use crate::native::DenialReason;
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_session::workspace::sandbox::workspace_write_sandbox_enforced;
#[cfg(target_os = "macos")]
use gents_session::workspace::sandbox::SANDBOX_EXEC;

/// Seatbelt cannot run these host programs: `ps` and `sudo` are setuid root,
/// which the kernel refuses to execute inside a sandbox, and launchd answers
/// `launchctl list` with an error to a sandboxed caller.
pub(crate) const SEATBELT_REFUSED_PROGRAMS: &[&str] =
    &["/bin/ps", "/usr/bin/sudo", "/bin/launchctl"];

pub(crate) const READ_ONLY_SEATBELT: &str = "macos_seatbelt_read_only";

pub(crate) fn select_sandbox(
    mode: CommandExecutionMode,
    workspace_write_sandbox_enforced: bool,
    program: &str,
) -> std::result::Result<&'static str, DenialReason> {
    match mode {
        CommandExecutionMode::ReadOnly
            if workspace_write_sandbox_enforced
                && !SEATBELT_REFUSED_PROGRAMS.contains(&program) =>
        {
            Ok(READ_ONLY_SEATBELT)
        }
        CommandExecutionMode::ReadOnly => Ok("policy_read_only"),
        CommandExecutionMode::Unrestricted => Ok("unsandboxed_unrestricted"),
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite
            if workspace_write_sandbox_enforced =>
        {
            Ok("macos_seatbelt")
        }
        CommandExecutionMode::ArtifactWrite => Err(DenialReason::ArtifactWriteSandboxUnavailable),
        CommandExecutionMode::WorkspaceWrite => Err(DenialReason::WorkspaceWriteSandboxUnavailable),
    }
}

pub(crate) fn read_only_reaches_network(command_key: &str) -> bool {
    matches!(command_key, "curl" | "tailscale")
}

pub(crate) fn kernel_network(
    mode: CommandExecutionMode,
    network_mode: CommandNetworkMode,
    reaches_network: bool,
) -> bool {
    let allows = network_mode.for_execution(mode).allows_network();
    match mode {
        CommandExecutionMode::ReadOnly => reaches_network && allows,
        _ => allows,
    }
}

#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SeatbeltWrites {
    Nothing,
    WritableRoot,
}

#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SeatbeltProcessInfo {
    SameSandbox,
    All,
}

#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeatbeltProfile {
    pub(crate) writes: SeatbeltWrites,
    pub(crate) network: bool,
    pub(crate) process_info: SeatbeltProcessInfo,
}

#[cfg(any(test, target_os = "macos"))]
fn write_capable_profile(network: bool) -> SeatbeltProfile {
    SeatbeltProfile {
        writes: SeatbeltWrites::WritableRoot,
        network,
        process_info: SeatbeltProcessInfo::SameSandbox,
    }
}

#[cfg(any(test, target_os = "macos"))]
pub(crate) fn seatbelt_profile(sandbox: &str, network: bool) -> Option<SeatbeltProfile> {
    match sandbox {
        "macos_seatbelt" => Some(write_capable_profile(network)),
        READ_ONLY_SEATBELT => Some(SeatbeltProfile {
            writes: SeatbeltWrites::Nothing,
            network,
            process_info: SeatbeltProcessInfo::All,
        }),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn rendered_profile(sandbox: &str, network: bool) -> Result<String> {
    seatbelt_profile(sandbox, network)
        .map(|profile| render_seatbelt_profile(&profile))
        .with_context(|| format!("sandbox {sandbox} has no seatbelt profile"))
}

struct ReadOnlyLaunch {
    program: String,
    args: Vec<String>,
    sandbox: &'static str,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    network: bool,
}

fn read_only_launch(
    root: &Path,
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<ReadOnlyLaunch, ToolError> {
    let resolved = admit_host_executable(command, root)
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let program = resolved.as_deref().unwrap_or(command);
    let sandbox = select_sandbox(policy.mode, workspace_write_sandbox_enforced(), program)
        .map_err(|reason| policy_denial(policy, reason))?;
    let key = executable_name_lookup_key(command).unwrap_or_else(|| command.to_owned());
    let contained = sandbox == READ_ONLY_SEATBELT;
    Ok(ReadOnlyLaunch {
        program: if contained { command } else { program }.to_owned(),
        args: read_only_launch_args(contained, &key, args),
        sandbox,
        network: kernel_network(
            policy.mode,
            policy.network_mode,
            read_only_reaches_network(&key),
        ),
    })
}

pub(super) fn artifact_policy_denial(message: impl Into<String>) -> ToolError {
    crate::native::command_observability::policy_denied();
    ToolError::reported_failure(FailureClass::PolicyDenied, message.into())
}

pub(super) struct CommandLaunch {
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) environment: RapidHashMap<String, String>,
    pub(super) sandbox: &'static str,
    pub(super) writable_root: Option<PathBuf>,
    pub(super) protection: Option<gents_pack::plugin::allowed::CommandProtection>,
    #[cfg(target_os = "macos")]
    pub(super) credentials: Option<command_credentials::CredentialProtection>,
}

impl CommandLaunch {
    pub(super) async fn revalidate(&self, call: &gents_loop::tool::ToolCallEnvelope) -> Result<()> {
        if let Some(protection) = &self.protection {
            protection.revalidate()?;
        }
        #[cfg(target_os = "macos")]
        if let Some(credentials) = &self.credentials {
            credentials.revalidate(call).await?;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = call;
        Ok(())
    }
}

pub(super) async fn prepare_command_launch(
    call: &gents_loop::tool::ToolCallEnvelope,
    source_root: &Path,
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<CommandLaunch, ToolError> {
    let mut environment = build_shell_env();
    let artifact = if policy.mode == CommandExecutionMode::ArtifactWrite {
        let scope = crate::runtime_context::tool_runtime_context(call).ok_or_else(|| {
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
        let recorder = gents_session::exec::ownership::process_recorder(call);
        grant
            .validate_for_launch(recorder.as_ref())
            .await
            .map_err(|error| {
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
    let workspace_root = crate::plugin::approval::workspace::command_writable_root(
        crate::runtime_context::host(call).approval.as_ref(),
        source_root,
        policy,
    )
    .map_err(command_guard_error)?;
    let writable_root = artifact
        .as_ref()
        .map(|grant| grant.root())
        .unwrap_or(&workspace_root);
    if policy.mode == CommandExecutionMode::WorkspaceWrite {
        command_temp::prepare(writable_root, &mut environment).map_err(command_guard_error)?;
    }
    let protection = crate::plugin::approval::workspace::command_guard_home(
        crate::runtime_context::host(call).approval.as_ref(),
    )
    .map_err(command_guard_error)?
    .map(|home| {
        gents_pack::plugin::allowed::CommandProtection::capture(
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
        let credentials = command_credentials::capture(call)
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
    crate::native::command_observability::policy_denied();
    ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
}

pub(crate) async fn prepare_declared_cli_command(
    call: &gents_loop::tool::ToolCallEnvelope,
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
    let protection = crate::plugin::approval::workspace::command_guard_home(
        crate::runtime_context::host(call).approval.as_ref(),
    )
    .map_err(command_guard_error)?
    .map(|home| {
        gents_pack::plugin::allowed::CommandProtection::capture(
            &home,
            workspace_write_sandbox_enforced(),
        )
    })
    .transpose()
    .map_err(command_guard_error)?;
    let argv: Vec<_> = std::iter::once(command.to_owned())
        .chain(args.iter().cloned())
        .collect();
    crate::plugin::approval::workspace::authorize_command(call, &argv, cwd, cwd, None, &policy)
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
    protection: &gents_pack::plugin::allowed::CommandProtection,
) -> Result<(String, Vec<String>, &'static str)> {
    let bounded = matches!(
        policy.mode,
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite
    );
    let read_only = (policy.mode == CommandExecutionMode::ReadOnly)
        .then(|| read_only_launch(root, command, args, policy))
        .transpose()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let (mut profile, sandbox, command, args) = match &read_only {
        Some(launch) if launch.sandbox == READ_ONLY_SEATBELT => (
            rendered_profile(launch.sandbox, launch.network)?,
            READ_ONLY_SEATBELT,
            launch.program.as_str(),
            launch.args.as_slice(),
        ),
        Some(launch) => (
            "(version 1)\n(allow default)\n".to_owned(),
            "macos_operator_state_guard",
            launch.program.as_str(),
            launch.args.as_slice(),
        ),
        None if bounded => (
            macos_workspace_write_policy(policy.effective_network_mode()),
            "macos_seatbelt",
            command,
            args,
        ),
        None => (
            "(version 1)\n(allow default)\n".to_owned(),
            "macos_operator_state_guard",
            command,
            args,
        ),
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
    Ok((SANDBOX_EXEC.to_owned(), argv, sandbox))
}

#[cfg(not(target_os = "macos"))]
fn protected_command_for_policy(
    _root: &Path,
    _command: &str,
    _args: &[String],
    _policy: &CommandExecutionPolicy,
    _protection: &gents_pack::plugin::allowed::CommandProtection,
) -> Result<(String, Vec<String>, &'static str)> {
    anyhow::bail!("Command isolation for runtime permissions is unavailable on this platform.")
}

fn sandboxed_command_for_policy(
    root: &Path,
    command_name: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(String, Vec<String>, &'static str), ToolError> {
    if policy.mode == CommandExecutionMode::ReadOnly {
        let launch = read_only_launch(root, command_name, args, policy)?;
        if launch.sandbox == READ_ONLY_SEATBELT {
            return read_only_seatbelt_command(launch).map_err(Into::into);
        }
        return Ok((launch.program, launch.args, launch.sandbox));
    }
    let sandbox = select_sandbox_for_execution(policy)?;
    match policy.mode {
        CommandExecutionMode::ReadOnly | CommandExecutionMode::Unrestricted => {
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
    select_sandbox(policy.mode, workspace_write_sandbox_enforced(), "")
        .map_err(|reason| policy_denial(policy, reason))
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
fn read_only_seatbelt_command(
    launch: ReadOnlyLaunch,
) -> Result<(String, Vec<String>, &'static str)> {
    let profile = rendered_profile(launch.sandbox, launch.network)?;
    let argv = ["-p".to_owned(), profile, "--".to_owned(), launch.program]
        .into_iter()
        .chain(launch.args)
        .collect();
    Ok((SANDBOX_EXEC.to_owned(), argv, launch.sandbox))
}

#[cfg(not(target_os = "macos"))]
fn read_only_seatbelt_command(
    _launch: ReadOnlyLaunch,
) -> Result<(String, Vec<String>, &'static str)> {
    anyhow::bail!("the read-only command sandbox requires macOS seatbelt on this build")
}

#[cfg(target_os = "macos")]
pub(super) fn macos_workspace_write_policy(network_mode: CommandNetworkMode) -> String {
    render_seatbelt_profile(&write_capable_profile(network_mode.allows_network()))
}

/// A read-only profile opens no file for writing except `/dev/null`, so it
/// also refuses creating, renaming, linking, unlinking and changing the mode,
/// owner or times of any file. Processes it starts inherit it.
///
/// Chromium registers its parent/child rendezvous before opening a page. These
/// bootstrap names include its bundle ID and PID; a writable profile permits
/// only the rendezvous namespaces, not unrelated global services or additional
/// filesystem access.
/// See [Chromium's rendezvous owner](https://chromium.googlesource.com/chromium/src/+/main/base/apple/mach_port_rendezvous_mac.cc).
#[cfg(target_os = "macos")]
pub(super) fn render_seatbelt_profile(profile: &SeatbeltProfile) -> String {
    let writable = profile.writes == SeatbeltWrites::WritableRoot;
    let process_info = match profile.process_info {
        SeatbeltProcessInfo::SameSandbox => "(allow process-info* (target same-sandbox))",
        SeatbeltProcessInfo::All => "(allow process-info*)",
    };
    let writable_rules = if writable {
        r#"(allow mach-register
    (global-name-prefix "org.chromium.Chromium.MachPortRendezvousServer.")
    (global-name-prefix "com.google.chrome.for.testing.MachPortRendezvousServer."))
(allow ipc-posix-shm-write*)
(allow file-write* (subpath (param "WRITABLE_ROOT")))
"#
    } else {
        ""
    };
    let network_rules = match (profile.network, writable) {
        (false, _) => "",
        (true, true) => "(allow network-outbound)\n(allow network-inbound)\n",
        (true, false) => "(allow network-outbound)\n",
    };
    format!(
        r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
{process_info}
(allow sysctl-read)
(allow mach-lookup)
(allow ipc-posix-sem)
(allow ipc-posix-shm-read*)
(allow file-read*)
(allow file-write-data (literal "/dev/null"))
{writable_rules}{network_rules}"#
    )
}
