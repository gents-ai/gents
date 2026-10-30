use super::super::context::ToolError;
use super::command_validation::{
    executable_name_lookup_key, policy_denial, read_only_argument_denial,
};
use super::CommandExecutionPolicy;
use crate::toolset::DenialReason;
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
        "status" | "ip" | "netcheck" | "version" | "ping" => Ok(()),
        other => Err(policy_denial(
            policy,
            DenialReason::ReadOnlySubcommandNotAllowlisted {
                command: "tailscale".to_string(),
                subcommand: other.to_string(),
            },
        )),
    }
}

pub(super) fn validate_curl_args(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
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
            return Err(read_only_argument_denial(policy, "curl", arg));
        }
    }

    if !has_http_url {
        return Err(policy_denial(
            policy,
            DenialReason::ReadOnlyUrlRequired {
                command: "curl".to_string(),
            },
        ));
    }

    Ok(())
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
    validate_git_read_only_flags(subcommand_args, policy)?;

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
    validate_git_read_only_flags(subcommand_args, policy)?;

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
    const UNSAFE_WITHOUT_ARGS: &[&str] = &["--search-zip", "-z"];
    for arg in args {
        if UNSAFE_WITHOUT_ARGS.contains(&arg.as_str())
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

fn validate_git_read_only_flags(
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
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
            return Err(read_only_argument_denial(policy, "git", arg));
        }
    }
    Ok(())
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
