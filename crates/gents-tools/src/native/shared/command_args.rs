use super::super::context::ToolError;
use super::command_options::{
    abbreviates_any_long, abbreviates_long, cluster_takes_next, go_flag_name, short_cluster_denied,
};
use super::command_validation::{
    executable_name_lookup_key, policy_denial, read_only_argument_denial,
};
use super::CommandExecutionPolicy;
use crate::native::DenialReason;
use std::path::Path;

pub(super) fn validate_launchctl_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let subcommand = args.first().map(String::as_str).ok_or_else(|| {
        policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandRequired {
                command: "launchctl".to_string(),
            },
        )
    })?;

    match subcommand {
        "list" | "print" | "print-disabled" | "blame" => Ok(()),
        other => Err(policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandNotAllowlisted {
                command: "launchctl".to_string(),
                subcommand: other.to_string(),
            },
        )),
    }
}

pub(super) fn validate_tailscale_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let subcommand = args.first().map(String::as_str).ok_or_else(|| {
        policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandRequired {
                command: "tailscale".to_string(),
            },
        )
    })?;

    match subcommand {
        "status" | "ip" | "netcheck" | "version" | "ping" => {
            first_denied(&args[1..], policy, "tailscale", |arg| {
                go_flag_name(arg).is_some_and(|name| matches!(name, "web" | "listen" | "browser"))
            })
        }
        other => Err(policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandNotAllowlisted {
                command: "tailscale".to_string(),
                subcommand: other.to_string(),
            },
        )),
    }
}

const CURL_DENIED_LONG_OPTIONS: &[&str] = &[
    "alt-svc",
    "config",
    "cookie-jar",
    "create-dirs",
    "data",
    "data-ascii",
    "data-binary",
    "data-raw",
    "data-urlencode",
    "dump-header",
    "etag-save",
    "form",
    "form-string",
    "hsts",
    "json",
    "libcurl",
    "next",
    "output",
    "output-dir",
    "proto",
    "proto-default",
    "proto-redir",
    "remote-header-name",
    "remote-name",
    "remote-name-all",
    "request",
    "ssl-sessions",
    "stderr",
    "trace",
    "trace-ascii",
    "upload-file",
];
const CURL_DENIED_SHORT_OPTIONS: &[char] =
    &['c', 'd', 'D', 'F', 'J', 'K', 'o', 'O', 'Q', 'T', 'X', ':'];
const CURL_VALUED_SHORT_OPTIONS: &[char] = &[
    'A', 'b', 'C', 'e', 'E', 'H', 'm', 'P', 'r', 't', 'u', 'U', 'w', 'x', 'y', 'Y', 'z',
];

fn first_denied(
    args: &[String],
    policy: &CommandExecutionPolicy,
    command: &str,
    denied: impl Fn(&str) -> bool,
) -> std::result::Result<(), ToolError> {
    match args.iter().find(|arg| denied(arg.as_str())) {
        Some(arg) => Err(read_only_argument_denial(policy, command, arg)),
        None => Ok(()),
    }
}

pub(super) fn validate_sed_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "sed", |arg| {
        abbreviates_long(arg, "in-place", 1)
            || short_cluster_denied(arg, &['i', 'I'], &['e', 'l', 'f'])
    })
}

pub(super) fn validate_curl_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "curl", |arg| {
        abbreviates_any_long(arg, CURL_DENIED_LONG_OPTIONS)
            || short_cluster_denied(arg, CURL_DENIED_SHORT_OPTIONS, CURL_VALUED_SHORT_OPTIONS)
            || arg.contains("%output")
    })?;
    if !args
        .iter()
        .any(|arg| arg.starts_with("http://") || arg.starts_with("https://"))
    {
        return Err(policy_denial(
            policy,
            DenialReason::ReadOnlyUrlRequired {
                command: "curl".to_string(),
            },
        ));
    }
    Ok(())
}

pub(super) fn validate_file_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "file", |arg| {
        abbreviates_long(arg, "compile", 1)
            || short_cluster_denied(arg, &['C'], &['e', 'f', 'F', 'm', 'P'])
    })
}

pub(super) fn validate_lsof_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "lsof", |arg| arg.starts_with("-D"))
}

const DATE_VALUED_SHORT_OPTIONS: &[char] = &['d', 'f', 'r', 'v', 'z'];

fn date_takes_next(arg: &str) -> bool {
    cluster_takes_next(arg, DATE_VALUED_SHORT_OPTIONS)
        || (!arg.contains('=') && abbreviates_any_long(arg, &["date", "file", "reference"]))
}

pub(super) fn validate_date_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "date", |arg| {
        abbreviates_long(arg, "set", 1)
            || short_cluster_denied(arg, &['s'], &['I', 'd', 'f', 'r', 'v', 'z'])
    })?;
    if args.iter().any(|arg| arg == "-j") {
        return Ok(());
    }
    let mut remaining = args.iter();
    while let Some(arg) = remaining.next() {
        if date_takes_next(arg) {
            remaining.next();
        } else if !arg.starts_with('-') && !arg.starts_with('+') {
            return Err(read_only_argument_denial(policy, "date", arg));
        }
    }
    Ok(())
}

pub(super) fn validate_hostname_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "hostname", |arg| {
        !arg.starts_with('-')
            || abbreviates_any_long(arg, &["file", "boot"])
            || short_cluster_denied(arg, &['F', 'b'], &[])
    })
}

pub(super) fn validate_sudo_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let command = args.first().map(String::as_str).ok_or_else(|| {
        policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandRequired {
                command: "sudo".to_string(),
            },
        )
    })?;
    let command_name = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command);

    match command_name {
        "launchctl" if command == "/bin/launchctl" => validate_launchctl_args(&args[1..], policy),
        "launchctl" => Err(read_only_argument_denial(policy, "sudo", command)),
        other => Err(policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandNotAllowlisted {
                command: "sudo".to_string(),
                subcommand: other.to_string(),
            },
        )),
    }
}

pub(super) fn validate_git_worktree_diff_policy(
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let command_key = executable_name_lookup_key(command).unwrap_or_else(|| command.to_string());
    if command_key == "git" {
        return deny_git_metadata_write_args(args, policy);
    }
    if let Some(script) = shell_command_script(command, args) {
        for git_args in git_invocations_in_script(script) {
            deny_git_metadata_write_args(&git_args, policy)?;
        }
    }
    Ok(())
}

fn deny_git_metadata_write_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    if let Some(argument) = args
        .iter()
        .map(String::as_str)
        .find(|arg| git_global_option_requires_denial(arg))
    {
        return Err(git_metadata_write_denial(policy, argument));
    }

    let (subcommand_idx, subcommand) = find_git_subcommand(args).ok_or_else(|| {
        policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandRequired {
                command: "git".to_string(),
            },
        )
    })?;
    let subcommand_args = &args[subcommand_idx + 1..];
    validate_git_read_only_flags(subcommand, subcommand_args, policy)?;

    if git_read_subcommand_allowed_under_worktree_diff(subcommand) {
        if subcommand == "branch" {
            return validate_git_branch_args(subcommand_args, policy);
        }
        return Ok(());
    }
    Err(git_metadata_write_denial(policy, subcommand))
}

fn git_read_subcommand_allowed_under_worktree_diff(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "status" | "diff" | "show" | "log" | "ls-files" | "grep" | "rev-parse" | "branch"
    )
}

fn git_metadata_write_denial(policy: &CommandExecutionPolicy, subcommand: &str) -> ToolError {
    policy_denial(
        policy,
        DenialReason::GitMetadataWriteDenied {
            command: "git".to_string(),
            subcommand: subcommand.to_string(),
        },
    )
}

fn shell_command_script<'a>(command: &str, args: &'a [String]) -> Option<&'a str> {
    let name = executable_name_lookup_key(command)?;
    if !matches!(name.as_str(), "sh" | "bash" | "zsh" | "dash" | "ksh") {
        return None;
    }
    let mut expect_script = false;
    let mut script = None;
    for arg in args {
        if expect_script {
            return Some(arg.as_str());
        }
        if arg == "-c" || arg == "-lc" || arg == "-cl" {
            expect_script = true;
            continue;
        }
        if arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c') {
            expect_script = true;
        }
        script = Some(arg.as_str());
    }
    expect_script.then_some(script).flatten()
}

fn git_invocations_in_script(script: &str) -> Vec<Vec<String>> {
    let mut invocations = Vec::new();
    for segment in script.split([';', '\n', '|', '&']) {
        let words: Vec<String> = segment.split_whitespace().map(str::to_string).collect();
        let Some(git_idx) = words.iter().position(|word| {
            executable_name_lookup_key(word)
                .as_deref()
                .is_some_and(|name| name == "git")
        }) else {
            continue;
        };
        invocations.push(words[git_idx + 1..].to_vec());
    }
    invocations
}

pub(super) fn validate_git_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    if let Some(argument) = args
        .iter()
        .map(String::as_str)
        .find(|arg| git_global_option_requires_denial(arg))
    {
        return Err(read_only_argument_denial(policy, "git", argument));
    }

    let (subcommand_idx, subcommand) = find_git_subcommand(args).ok_or_else(|| {
        policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandRequired {
                command: "git".to_string(),
            },
        )
    })?;
    let subcommand_args = &args[subcommand_idx + 1..];
    validate_git_read_only_flags(subcommand, subcommand_args, policy)?;

    match subcommand {
        "status" | "diff" | "show" | "log" | "ls-files" | "grep" | "rev-parse" => Ok(()),
        "branch" => validate_git_branch_args(subcommand_args, policy),
        other => Err(policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandNotAllowlisted {
                command: "git".to_string(),
                subcommand: other.to_string(),
            },
        )),
    }
}

pub(super) fn validate_ripgrep_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    const UNSAFE_WITH_ARGS: &[&str] = &["--pre", "--hostname-bin"];
    const VALUED: &[char] = &[
        'A', 'B', 'C', 'd', 'e', 'E', 'f', 'g', 'j', 'm', 'M', 'r', 't', 'T',
    ];
    for arg in args {
        if arg == "--search-zip"
            || short_cluster_denied(arg, &['z'], VALUED)
            || UNSAFE_WITH_ARGS
                .iter()
                .any(|option| arg == option || arg.starts_with(&format!("{option}=")))
        {
            return Err(read_only_argument_denial(policy, "rg", arg));
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

fn git_read_only_flag_denied(subcommand: &str, arg: &str) -> bool {
    const UNSAFE_GIT_FLAGS: &[&str] = &[
        "--output",
        "--ext-diff",
        "--exec",
        "--paginate",
        "--show-signature",
    ];
    UNSAFE_GIT_FLAGS.contains(&arg)
        || arg.starts_with("--output=")
        || arg.starts_with("--exec=")
        || abbreviates_long(arg, "textconv", 5)
        || arg.contains("%G")
        || (subcommand == "grep"
            && (abbreviates_long(arg, "open-files-in-pager", 1)
                || short_cluster_denied(arg, &['O'], &['A', 'B', 'C', 'e', 'f', 'm'])))
        || (subcommand == "status"
            && (abbreviates_long(arg, "verbose", 1)
                || short_cluster_denied(arg, &['v'], &['u', 'M'])))
}

fn validate_git_read_only_flags(
    subcommand: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    first_denied(args, policy, "git", |arg| {
        git_read_only_flag_denied(subcommand, arg)
    })
}

fn validate_git_branch_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    if args.is_empty() {
        return Ok(());
    }

    for arg in args {
        match arg.as_str() {
            "--list" | "-l" | "--show-current" | "-a" | "--all" | "-r" | "--remotes" | "-v"
            | "-vv" | "--verbose" => {}
            _ if arg.starts_with("--format=") => {}
            _ => return Err(read_only_argument_denial(policy, "git", arg)),
        }
    }
    Ok(())
}

const GIT_READ_ONLY_GLOBAL_OPTIONS: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "log.showSignature=false",
    "--no-optional-locks",
];

pub(crate) fn read_only_launch_args(
    kernel_contained: bool,
    command_key: &str,
    args: &[String],
) -> Vec<String> {
    let owned = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    };
    match command_key {
        "git" => {
            let mut launched = owned(GIT_READ_ONLY_GLOBAL_OPTIONS);
            let mut inserted = false;
            for arg in args {
                launched.push(arg.clone());
                if !inserted && !arg.starts_with('-') {
                    inserted = true;
                    if matches!(arg.as_str(), "diff" | "show" | "log") {
                        launched.extend(owned(&["--no-ext-diff", "--no-textconv"]));
                    }
                }
            }
            launched
        }
        "curl" => owned(&["--proto", "=http,https", "--proto-redir", "=http,https"])
            .into_iter()
            .chain(args.iter().cloned())
            .collect(),
        "sed" if !kernel_contained => std::iter::once("--sandbox".to_owned())
            .chain(args.iter().cloned())
            .collect(),
        _ => args.to_vec(),
    }
}
