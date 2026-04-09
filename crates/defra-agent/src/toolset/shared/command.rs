use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::process::Command;

use super::context::ToolContext;
use super::filesystem::truncate_text;

pub(crate) async fn run_command(
    context: &ToolContext,
    command_name: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let cwd = context.resolve_existing_dir(cwd)?;
    let mut command = Command::new(command_name);
    command
        .args(args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb");

    let output = tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("timed out after {}s", timeout.as_secs()))??;

    let stdout = truncate_text(
        &String::from_utf8_lossy(&output.stdout),
        super::super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let stderr = truncate_text(
        &String::from_utf8_lossy(&output.stderr),
        super::super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let exit_code = output.status.code().unwrap_or(-1);
    let command_line = std::iter::once(command_name)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(format!(
        "cwd: {}\ncommand: {}\nexit_code: {}\nstdout:\n{}\nstderr:\n{}",
        context.display_path(&cwd),
        command_line,
        exit_code,
        if stdout.is_empty() {
            "(empty)"
        } else {
            &stdout
        },
        if stderr.is_empty() {
            "(empty)"
        } else {
            &stderr
        },
    ))
}

pub(crate) fn validate_read_only_command(
    command: &str,
    args: &[String],
    allowlist: &[String],
) -> Result<()> {
    if !allowlist.iter().any(|allowed| allowed == command) {
        bail!("command is not allowed by the read-only bash tool: {command}");
    }

    match command {
        "sed" => {
            if args
                .iter()
                .any(|arg| arg == "-i" || arg == "--in-place" || arg.starts_with("-i"))
            {
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
                        | "-fprintf"
                        | "-fls"
                )
            }) {
                bail!("find arguments that can write or execute are not allowed");
            }
        }
        "git" => validate_git_args(args)?,
        _ => {}
    }

    Ok(())
}

fn validate_git_args(args: &[String]) -> Result<()> {
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("git requires a read-only subcommand"))?;

    match subcommand {
        "status" | "diff" | "show" | "log" | "ls-files" | "grep" | "branch" | "rev-parse" => Ok(()),
        other => bail!("git subcommand is not allowed by the read-only bash tool: {other}"),
    }
}
