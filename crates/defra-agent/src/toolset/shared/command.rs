use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use tokio::process::Command;

use super::context::ToolContext;

const OUTPUT_META_PREFIX: &str = "defra_exec: ";
const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const CORE_ENV_VARS: &[&str] = &[
    "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME",
    "USER",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecutionMode {
    ReadOnly,
    WorkspaceWrite,
    Unrestricted,
}

impl CommandExecutionMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "read_only" | "ReadOnly" => Ok(Self::ReadOnly),
            // Compatibility alias: managed_write currently has the same enforced
            // runtime contract as workspace_write, so normalize it at parse time.
            "workspace_write" | "WorkspaceWrite" | "managed_write" | "ManagedWrite" => {
                Ok(Self::WorkspaceWrite)
            }
            "unrestricted" | "Unrestricted" => Ok(Self::Unrestricted),
            other => bail!("unknown command execution policy mode {other}"),
        }
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

    fn allows_network(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecutionPolicy {
    pub mode: CommandExecutionMode,
    pub allowed_argv_prefixes: Vec<Vec<String>>,
    pub forbidden_argv_prefixes: Vec<Vec<String>>,
    pub network_mode: CommandNetworkMode,
    read_only_allowlist: Vec<String>,
}

impl CommandExecutionPolicy {
    pub fn read_only(allowlist: Vec<String>) -> Self {
        Self {
            mode: CommandExecutionMode::ReadOnly,
            allowed_argv_prefixes: Vec::new(),
            forbidden_argv_prefixes: Vec::new(),
            network_mode: CommandNetworkMode::Inherit,
            read_only_allowlist: allowlist,
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

    pub fn with_network_mode(mut self, network_mode: CommandNetworkMode) -> Self {
        self.network_mode = network_mode;
        self
    }
}

pub(crate) async fn run_command(
    context: &ToolContext,
    command_name: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
    policy: &CommandExecutionPolicy,
    raw_json: bool,
) -> Result<String> {
    let cwd = context.resolve_existing_dir(cwd)?;
    let argv = std::iter::once(command_name.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let command_line = shell_join(&argv);
    let (program, command_args, sandbox) =
        sandboxed_command_for_policy(context.root(), command_name, args, policy)?;
    let mut command = Command::new(program);
    command
        .args(command_args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(build_shell_env())
        .kill_on_drop(true);

    let started = Instant::now();
    let output = tokio::time::timeout(timeout, command.output()).await;
    let duration_ms = elapsed_ms(started);
    let (exit_code, timed_out, stdout_raw, stderr_raw) = match output {
        Ok(output) => {
            let output = output?;
            (
                output.status.code(),
                false,
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        }
        Err(_) => (None, true, String::new(), String::new()),
    };

    let stdout = truncate_stream(&stdout_raw, super::super::DEFAULT_MAX_COMMAND_CHARS);
    let stderr = truncate_stream(&stderr_raw, super::super::DEFAULT_MAX_COMMAND_CHARS);
    let status = if timed_out {
        "timeout"
    } else if exit_code == Some(0) {
        "success"
    } else {
        "exit_nonzero"
    };
    let metadata = CommandMetadata {
        ok: !timed_out && exit_code == Some(0),
        status,
        command: command_line,
        argv,
        cwd: context.display_path(&cwd),
        exit_code,
        timed_out,
        duration_ms,
        timeout_ms: millis(timeout),
        execution_mode: policy.mode,
        network_mode: policy.network_mode,
        sandbox,
        stdout_truncation: stdout.metadata,
        stderr_truncation: stderr.metadata,
    };
    let output = CommandOutput {
        metadata,
        stdout: stdout.content,
        stderr: stderr.content,
    };

    render_command_output(&output, raw_json)
}

#[cfg(test)]
pub(crate) fn validate_read_only_command(
    command: &str,
    args: &[String],
    allowlist: &[String],
) -> Result<()> {
    let policy = CommandExecutionPolicy::read_only(allowlist.to_vec());
    validate_command_policy(command, args, &policy)
}

pub(crate) fn validate_command_policy(
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> Result<()> {
    let argv = std::iter::once(command.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();

    if let Some(prefix) = first_matching_prefix(&argv, &policy.forbidden_argv_prefixes) {
        bail!(
            "command is forbidden by command execution policy prefix: {}",
            shell_join(prefix)
        );
    }

    if !policy.allowed_argv_prefixes.is_empty()
        && first_matching_prefix(&argv, &policy.allowed_argv_prefixes).is_none()
    {
        bail!(
            "command is not allowed by command execution policy prefixes: {}",
            shell_join(&argv)
        );
    }

    validate_network_mode(command, args, policy)?;

    if matches!(policy.mode, CommandExecutionMode::ReadOnly) {
        validate_read_only_command_inner(command, args, &policy.read_only_allowlist)?;
    }

    Ok(())
}

pub(crate) fn parse_argv_prefixes(values: &[String]) -> Result<Vec<Vec<String>>> {
    values
        .iter()
        .map(|value| parse_argv_prefix(value))
        .collect::<Result<Vec<_>>>()
}

pub(crate) fn build_shell_env() -> HashMap<String, String> {
    build_shell_env_from_vars(std::env::vars())
}

pub(crate) fn build_shell_env_from_vars<I>(vars: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env = vars
        .into_iter()
        .filter(|(key, _)| {
            CORE_ENV_VARS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(key))
        })
        .filter(|(key, _)| !is_secret_env_name(key))
        .collect::<HashMap<_, _>>();

    env.entry("PATH".to_string())
        .or_insert_with(|| FALLBACK_PATH.to_string());
    env.insert("PAGER".to_string(), "cat".to_string());
    env.insert("GIT_PAGER".to_string(), "cat".to_string());
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("CLICOLOR".to_string(), "0".to_string());
    env.insert("TERM".to_string(), "dumb".to_string());
    env
}

fn validate_read_only_command_inner(
    command: &str,
    args: &[String],
    allowlist: &[String],
) -> Result<()> {
    let command_key = executable_name_lookup_key(command).unwrap_or_else(|| command.to_string());
    if !allowlist.iter().any(|allowed| {
        allowed == command
            || executable_name_lookup_key(allowed)
                .as_deref()
                .is_some_and(|allowed_key| allowed_key == command_key)
    }) {
        bail!("command is not allowed by the read-only bash tool: {command}");
    }

    match command_key.as_str() {
        "sed" => {
            if args.iter().any(|arg| {
                arg == "-i"
                    || arg == "--in-place"
                    || arg.starts_with("-i")
                    || arg.starts_with("--in-place=")
            }) {
                bail!("sed in-place edits are not allowed");
            }
        }
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fprint"
                        | "-fprint0"
                        | "-fprintf"
                        | "-fls"
                )
            }) {
                bail!("find arguments that can write or execute are not allowed");
            }
        }
        "git" => validate_git_args(args)?,
        "rg" => validate_ripgrep_args(args)?,
        "launchctl" => validate_launchctl_args(args)?,
        "tailscale" => validate_tailscale_args(args)?,
        "curl" => validate_curl_args(args)?,
        "sudo" => validate_sudo_args(args)?,
        _ => {}
    }

    Ok(())
}

fn parse_argv_prefix(value: &str) -> Result<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("argv prefix cannot be empty");
    }

    if trimmed.starts_with('[') {
        let prefix = serde_json::from_str::<Vec<String>>(trimmed)
            .with_context(|| format!("parsing argv prefix JSON {trimmed}"))?;
        if prefix.is_empty() || prefix.iter().any(|token| token.trim().is_empty()) {
            bail!("argv prefix must contain non-empty tokens");
        }
        return Ok(prefix);
    }

    let prefix = trimmed
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if prefix.is_empty() {
        bail!("argv prefix cannot be empty");
    }
    Ok(prefix)
}

fn first_matching_prefix<'a>(
    argv: &[String],
    prefixes: &'a [Vec<String>],
) -> Option<&'a Vec<String>> {
    prefixes.iter().find(|prefix| {
        argv.len() >= prefix.len() && argv.iter().zip(prefix.iter()).all(|(a, b)| a == b)
    })
}

fn executable_name_lookup_key(raw: &str) -> Option<String> {
    Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

fn is_secret_env_name(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key.contains("KEY") || key.contains("SECRET") || key.contains("TOKEN")
}

fn validate_network_mode(
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> Result<()> {
    if !matches!(policy.network_mode, CommandNetworkMode::Disabled) {
        return Ok(());
    }

    match policy.mode {
        CommandExecutionMode::WorkspaceWrite => Ok(()),
        CommandExecutionMode::Unrestricted => {
            bail!("command_network_mode=disabled cannot be enforced for unrestricted bash")
        }
        CommandExecutionMode::ReadOnly => validate_read_only_network_disabled(command, args),
    }
}

fn validate_read_only_network_disabled(command: &str, args: &[String]) -> Result<()> {
    let command_key = executable_name_lookup_key(command).unwrap_or_else(|| command.to_string());
    match command_key.as_str() {
        "curl" => bail!("curl is not allowed when command_network_mode=disabled"),
        "tailscale" => match args.first().map(String::as_str) {
            Some("ping" | "netcheck") => {
                bail!("tailscale network probes are not allowed when command_network_mode=disabled")
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

pub(in crate::toolset) fn select_sandbox_for_policy(
    mode: CommandExecutionMode,
    workspace_write_sandbox_enforced: bool,
) -> Result<&'static str> {
    match mode {
        CommandExecutionMode::ReadOnly => Ok("policy_read_only"),
        CommandExecutionMode::Unrestricted => Ok("unsandboxed_unrestricted"),
        CommandExecutionMode::WorkspaceWrite if workspace_write_sandbox_enforced => {
            Ok("macos_seatbelt")
        }
        CommandExecutionMode::WorkspaceWrite => {
            if cfg!(target_os = "macos") {
                bail!("macOS sandbox-exec is required for workspace_write bash but was not found")
            } else {
                bail!("workspace_write bash requires macOS seatbelt sandbox enforcement on this build")
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn workspace_write_sandbox_enforced() -> bool {
    Path::new(SANDBOX_EXEC).exists()
}

#[cfg(not(target_os = "macos"))]
fn workspace_write_sandbox_enforced() -> bool {
    false
}

fn sandboxed_command_for_policy(
    root: &Path,
    command_name: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> Result<(String, Vec<String>, &'static str)> {
    let sandbox = select_sandbox_for_policy(policy.mode, workspace_write_sandbox_enforced())?;
    match policy.mode {
        CommandExecutionMode::ReadOnly => Ok((command_name.to_string(), args.to_vec(), sandbox)),
        CommandExecutionMode::Unrestricted => {
            Ok((command_name.to_string(), args.to_vec(), sandbox))
        }
        CommandExecutionMode::WorkspaceWrite => sandboxed_workspace_write_command(
            root,
            command_name,
            args,
            policy.network_mode,
            sandbox,
        ),
    }
}

#[cfg(target_os = "macos")]
fn sandboxed_workspace_write_command(
    root: &Path,
    command_name: &str,
    args: &[String],
    network_mode: CommandNetworkMode,
    sandbox: &'static str,
) -> Result<(String, Vec<String>, &'static str)> {
    let policy = macos_workspace_write_policy(network_mode);
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
) -> Result<(String, Vec<String>, &'static str)> {
    bail!("workspace_write bash requires macOS seatbelt sandbox enforcement on this build")
}

#[cfg(target_os = "macos")]
fn macos_workspace_write_policy(network_mode: CommandNetworkMode) -> String {
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
(allow ipc-posix-sem)
(allow ipc-posix-shm-read*)
(allow ipc-posix-shm-write*)
(allow file-read*)
(allow file-write-data (literal "/dev/null"))
(allow file-write* (subpath (param "WRITABLE_ROOT")))
{network_policy}"#
    )
}

fn validate_launchctl_args(args: &[String]) -> Result<()> {
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("launchctl requires a read-only subcommand"))?;

    match subcommand {
        "list" | "print" | "print-disabled" | "blame" => Ok(()),
        other => bail!("launchctl subcommand is not allowed by the read-only bash tool: {other}"),
    }
}

fn validate_tailscale_args(args: &[String]) -> Result<()> {
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("tailscale requires a read-only subcommand"))?;

    match subcommand {
        "status" | "ip" | "netcheck" | "version" | "ping" => Ok(()),
        other => bail!("tailscale subcommand is not allowed by the read-only bash tool: {other}"),
    }
}

fn validate_curl_args(args: &[String]) -> Result<()> {
    let mut has_http_url = false;
    for arg in args {
        if arg.starts_with("http://") || arg.starts_with("https://") {
            has_http_url = true;
        }

        let mutating = matches!(
            arg.as_str(),
            "-d" | "--data"
                | "--data-raw"
                | "--data-binary"
                | "--data-urlencode"
                | "-F"
                | "--form"
                | "-T"
                | "--upload-file"
                | "-X"
                | "--request"
                | "-o"
                | "--output"
                | "-O"
                | "--remote-name"
                | "--remote-header-name"
                | "-K"
                | "--config"
                | "--next"
        ) || arg.starts_with("-d")
            || arg.starts_with("--data=")
            || arg.starts_with("-F")
            || arg.starts_with("--form=")
            || arg.starts_with("-T")
            || arg.starts_with("--upload-file=")
            || arg.starts_with("-X")
            || arg.starts_with("--request=")
            || arg.starts_with("-o")
            || arg.starts_with("--output=")
            || arg.starts_with("-O")
            || arg.starts_with("-K")
            || arg.starts_with("--config=");
        if mutating {
            bail!("curl argument is not allowed by the read-only bash tool: {arg}");
        }
    }

    if !has_http_url {
        bail!("curl requires an http:// or https:// URL in the read-only bash tool");
    }

    Ok(())
}

fn validate_sudo_args(args: &[String]) -> Result<()> {
    let command = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("sudo requires an approved command"))?;
    let command_name = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command);

    match command_name {
        "launchctl" if command == "/bin/launchctl" => validate_launchctl_args(&args[1..]),
        "launchctl" => {
            bail!("sudo launchctl must use the absolute /bin/launchctl path")
        }
        other => bail!("sudo command is not allowed by the read-only bash tool: {other}"),
    }
}

fn validate_git_args(args: &[String]) -> Result<()> {
    if args
        .iter()
        .map(String::as_str)
        .any(git_global_option_requires_denial)
    {
        bail!("git global options that redirect config or helper lookup are not allowed");
    }

    let (subcommand_idx, subcommand) =
        find_git_subcommand(args).ok_or_else(|| anyhow!("git requires a read-only subcommand"))?;
    let subcommand_args = &args[subcommand_idx + 1..];
    validate_git_read_only_flags(subcommand_args)?;

    match subcommand {
        "status" | "diff" | "show" | "log" | "ls-files" | "grep" | "rev-parse" => Ok(()),
        "branch" => validate_git_branch_args(subcommand_args),
        other => bail!("git subcommand is not allowed by the read-only bash tool: {other}"),
    }
}

fn validate_ripgrep_args(args: &[String]) -> Result<()> {
    const UNSAFE_WITH_ARGS: &[&str] = &["--pre", "--hostname-bin"];
    const UNSAFE_WITHOUT_ARGS: &[&str] = &["--search-zip", "-z"];
    for arg in args {
        if UNSAFE_WITHOUT_ARGS.contains(&arg.as_str())
            || UNSAFE_WITH_ARGS
                .iter()
                .any(|option| arg == option || arg.starts_with(&format!("{option}=")))
        {
            bail!("rg argument is not allowed by the read-only bash tool: {arg}");
        }
    }
    Ok(())
}

fn git_global_option_requires_denial(arg: &str) -> bool {
    matches!(
        arg,
        "-C" | "-c"
            | "--config-env"
            | "--exec-path"
            | "--git-dir"
            | "--namespace"
            | "--super-prefix"
            | "--work-tree"
    ) || ((arg.starts_with("-C") || arg.starts_with("-c")) && arg.len() > 2)
        || arg.starts_with("--config-env=")
        || arg.starts_with("--exec-path=")
        || arg.starts_with("--git-dir=")
        || arg.starts_with("--namespace=")
        || arg.starts_with("--super-prefix=")
        || arg.starts_with("--work-tree=")
}

fn find_git_subcommand(args: &[String]) -> Option<(usize, &str)> {
    let mut skip_next = false;
    for (idx, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }

        let arg = arg.as_str();
        if matches!(
            arg,
            "-C" | "-c"
                | "--config-env"
                | "--exec-path"
                | "--git-dir"
                | "--namespace"
                | "--super-prefix"
                | "--work-tree"
        ) {
            skip_next = true;
            continue;
        }
        if arg == "--" || arg.starts_with('-') {
            continue;
        }
        return Some((idx, arg));
    }
    None
}

fn validate_git_read_only_flags(args: &[String]) -> Result<()> {
    const UNSAFE_GIT_FLAGS: &[&str] = &[
        "--output",
        "--ext-diff",
        "--textconv",
        "--exec",
        "--paginate",
    ];
    for arg in args {
        if UNSAFE_GIT_FLAGS.contains(&arg.as_str())
            || arg.starts_with("--output=")
            || arg.starts_with("--exec=")
        {
            bail!("git argument is not allowed by the read-only bash tool: {arg}");
        }
    }
    Ok(())
}

fn validate_git_branch_args(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }

    for arg in args {
        match arg.as_str() {
            "--list" | "-l" | "--show-current" | "-a" | "--all" | "-r" | "--remotes" | "-v"
            | "-vv" | "--verbose" => {}
            _ if arg.starts_with("--format=") => {}
            _ => bail!("git branch argument is not read-only: {arg}"),
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct CommandOutput {
    #[serde(flatten)]
    metadata: CommandMetadata,
    stdout: String,
    stderr: String,
}

#[derive(Serialize)]
struct CommandMetadata {
    ok: bool,
    status: &'static str,
    command: String,
    argv: Vec<String>,
    cwd: String,
    exit_code: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
    timeout_ms: u64,
    execution_mode: CommandExecutionMode,
    network_mode: CommandNetworkMode,
    sandbox: &'static str,
    stdout_truncation: StreamTruncationMetadata,
    stderr_truncation: StreamTruncationMetadata,
}

#[derive(Clone, Copy, Serialize)]
struct StreamTruncationMetadata {
    returned_chars: usize,
    total_chars: usize,
    max_chars: usize,
    truncated: bool,
}

struct TruncatedStream {
    content: String,
    metadata: StreamTruncationMetadata,
}

fn truncate_stream(text: &str, max_chars: usize) -> TruncatedStream {
    let total_chars = text.chars().count();
    let truncated = total_chars > max_chars;
    let content = if truncated {
        let mut value = text.chars().take(max_chars).collect::<String>();
        value.push_str(&format!("\n[truncated to {max_chars} chars]"));
        value
    } else {
        text.to_string()
    };
    TruncatedStream {
        content,
        metadata: StreamTruncationMetadata {
            returned_chars: total_chars.min(max_chars),
            total_chars,
            max_chars,
            truncated,
        },
    }
}

fn render_command_output(output: &CommandOutput, raw_json: bool) -> Result<String> {
    if raw_json {
        return serde_json::to_string(output).context("serializing command output");
    }

    let mut out = String::from(OUTPUT_META_PREFIX);
    out.push_str(&serde_json::to_string(&output.metadata).context("serializing command metadata")?);
    out.push_str("\nstdout:\n");
    out.push_str(if output.stdout.is_empty() {
        "(empty)"
    } else {
        &output.stdout
    });
    out.push_str("\nstderr:\n");
    out.push_str(if output.stderr.is_empty() {
        "(empty)"
    } else {
        &output.stderr
    });
    Ok(out)
}

fn elapsed_ms(started: Instant) -> u64 {
    let millis = started.elapsed().as_millis();
    millis.min(u128::from(u64::MAX)) as u64
}

fn millis(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    millis.min(u128::from(u64::MAX)) as u64
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '='))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\"'\"'"))
}
