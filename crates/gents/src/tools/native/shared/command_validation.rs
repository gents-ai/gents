use std::path::Path;

use super::super::context::ToolError;
use super::command_args::{
    validate_curl_args, validate_git_args, validate_git_worktree_diff_policy,
    validate_launchctl_args, validate_ripgrep_args, validate_sudo_args, validate_tailscale_args,
};
use super::{CommandExecutionMode, CommandExecutionPolicy, CommandNetworkMode};
use crate::tools::native::{CommandPolicyDenial, DenialReason};

#[cfg(test)]
pub(crate) fn validate_read_only_command(
    command: &str,
    args: &[String],
    allowlist: &[String],
) -> std::result::Result<(), ToolError> {
    let policy = CommandExecutionPolicy::read_only(allowlist.to_vec());
    validate_command_policy(command, args, &policy)
}

pub(crate) fn validate_command_policy(
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    validate_command_policy_inner(command, args, None, policy)
}

pub(super) fn validate_command_policy_with_resolved_executable(
    command: &str,
    args: &[String],
    resolved_command: &str,
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    validate_command_policy_inner(command, args, Some(resolved_command), policy)
}

fn validate_command_policy_inner(
    command: &str,
    args: &[String],
    resolved_command: Option<&str>,
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let argv = std::iter::once(command.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let resolved_argv = resolved_command.map(|resolved| {
        std::iter::once(resolved.to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
    });

    if let Some(prefix) =
        first_matching_prefix(&argv, &policy.forbidden_argv_prefixes).or_else(|| {
            resolved_argv
                .as_ref()
                .and_then(|argv| first_matching_prefix(argv, &policy.forbidden_argv_prefixes))
        })
    {
        return Err(policy_denial(
            policy,
            DenialReason::ForbiddenPrefix {
                matched: prefix.clone(),
            },
        ));
    }

    if policy.deny_all_argv {
        return Err(policy_denial(
            policy,
            DenialReason::AllowedPrefixRequired { argv: argv.clone() },
        ));
    }

    let allowed_prefix_matched = first_matching_prefix(&argv, &policy.allowed_argv_prefixes)
        .is_some()
        || resolved_argv.as_ref().is_some_and(|argv| {
            first_matching_prefix(argv, &policy.allowed_argv_prefixes).is_some()
        });
    if !policy.allowed_argv_prefixes.is_empty() && !allowed_prefix_matched {
        return Err(policy_denial(
            policy,
            DenialReason::AllowedPrefixRequired { argv },
        ));
    }

    validate_network_mode(command, args, policy)?;

    if matches!(policy.mode, CommandExecutionMode::ReadOnly) {
        validate_read_only_command_inner(
            command,
            args,
            &policy.read_only_allowlist,
            allowed_prefix_matched,
            policy,
        )?;
    } else if policy.deny_git_metadata_writes {
        validate_git_worktree_diff_policy(command, args, policy)?;
    }

    Ok(())
}

fn validate_read_only_command_inner(
    command: &str,
    args: &[String],
    allowlist: &[String],
    allowed_prefix_matched: bool,
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let command_key = executable_name_lookup_key(command).unwrap_or_else(|| command.to_string());
    let built_in_allowlisted = allowlist.iter().any(|allowed| {
        allowed == command
            || executable_name_lookup_key(allowed)
                .as_deref()
                .is_some_and(|allowed_key| allowed_key == command_key)
    });
    if !built_in_allowlisted && !allowed_prefix_matched {
        return Err(policy_denial(
            policy,
            DenialReason::ReadOnlyCommandNotAllowlisted {
                command: command_key,
            },
        ));
    }

    match command_key.as_str() {
        "sed" => {
            if let Some(argument) = args.iter().find(|arg| {
                arg.as_str() == "-i"
                    || arg.as_str() == "--in-place"
                    || arg.starts_with("-i")
                    || arg.starts_with("--in-place=")
            }) {
                return Err(read_only_argument_denial(policy, "sed", argument));
            }
        }
        "find" => {
            if let Some(argument) = args.iter().find(|arg| {
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
                return Err(read_only_argument_denial(policy, "find", argument));
            }
        }
        "git" => validate_git_args(args, policy)?,
        "rg" => validate_ripgrep_args(args, policy)?,
        "launchctl" => validate_launchctl_args(args, policy)?,
        "tailscale" => validate_tailscale_args(args, policy)?,
        "curl" => validate_curl_args(args, policy)?,
        "sudo" => validate_sudo_args(args, policy)?,
        _ => {}
    }

    Ok(())
}

fn first_matching_prefix<'a>(
    argv: &[String],
    prefixes: &'a [Vec<String>],
) -> Option<&'a Vec<String>> {
    prefixes.iter().find(|prefix| {
        argv.len() >= prefix.len() && argv.iter().zip(prefix.iter()).all(|(a, b)| a == b)
    })
}

pub(super) fn policy_denial(policy: &CommandExecutionPolicy, reason: DenialReason) -> ToolError {
    ToolError::policy_denial(CommandPolicyDenial::new(
        reason,
        policy.mode,
        policy.network_mode,
    ))
}

pub(super) fn read_only_argument_denial(
    policy: &CommandExecutionPolicy,
    command: &str,
    argument: &str,
) -> ToolError {
    policy_denial(
        policy,
        DenialReason::ReadOnlyArgumentNotAllowed {
            command: command.to_string(),
            argument: argument.to_string(),
        },
    )
}

pub(super) fn executable_name_lookup_key(raw: &str) -> Option<String> {
    Path::new(raw)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

pub(super) fn validate_network_mode(
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    if !matches!(policy.network_mode, CommandNetworkMode::Disabled) {
        return Ok(());
    }

    match policy.mode {
        CommandExecutionMode::WorkspaceWrite | CommandExecutionMode::ArtifactWrite => Ok(()),
        CommandExecutionMode::Unrestricted => Err(policy_denial(
            policy,
            DenialReason::DisabledNetworkUnenforceable,
        )),
        CommandExecutionMode::ReadOnly => {
            validate_read_only_network_disabled(command, args, policy)
        }
    }
}

fn validate_read_only_network_disabled(
    command: &str,
    args: &[String],
    policy: &CommandExecutionPolicy,
) -> std::result::Result<(), ToolError> {
    let command_key = executable_name_lookup_key(command).unwrap_or_else(|| command.to_string());
    match command_key.as_str() {
        "curl" => Err(policy_denial(
            policy,
            DenialReason::DisabledNetworkCommand {
                command: "curl".to_string(),
            },
        )),
        "tailscale" => match args.first().map(String::as_str) {
            Some("ping" | "netcheck") => Err(policy_denial(
                policy,
                DenialReason::DisabledNetworkCommand {
                    command: "tailscale".to_string(),
                },
            )),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}
