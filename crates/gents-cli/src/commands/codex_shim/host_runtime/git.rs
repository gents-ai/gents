use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use gents_codex_protocol as codex;
use tokio::process::Command;

use crate::commands::codex_shim::{ShimState, JSONRPC_INTERNAL_ERROR};

use super::paths::resolve_runtime_cwd;
use super::HostRuntimeError;

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

pub(in crate::commands::codex_shim) async fn git_diff_to_remote(
    state: &ShimState,
    params: codex::GitDiffToRemoteParams,
) -> Result<codex::GitDiffToRemoteResponse, HostRuntimeError> {
    let cwd = resolve_runtime_cwd(state, Some(params.cwd.as_path()))?;
    let _repo_root = run_git(&cwd, &["rev-parse", "--show-toplevel"]).await?;
    let base_sha = match run_git(&cwd, &["merge-base", "HEAD", "@{upstream}"]).await {
        Ok(sha) => sha,
        Err(_) => run_git(&cwd, &["rev-parse", "HEAD"]).await?,
    };
    let base_sha = base_sha.trim().to_string();
    if base_sha.is_empty() {
        return Err(HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("failed to compute git base sha for {}", cwd.display()),
        });
    }

    let mut diff = run_git_allowing_diff_exit(
        &cwd,
        &["diff", "--no-textconv", "--no-ext-diff", base_sha.as_str()],
    )
    .await?;
    diff.push_str(&untracked_git_diff(&cwd).await?);

    Ok(codex::GitDiffToRemoteResponse {
        sha: codex::GitSha::new(&base_sha),
        diff,
    })
}

pub(in crate::commands::codex_shim) async fn thread_git_info(
    cwd: &Path,
) -> Option<serde_json::Value> {
    let sha = run_git(cwd, &["rev-parse", "HEAD"]).await.ok()?;
    let sha = sha.trim();
    if sha.is_empty() {
        return None;
    }

    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty() && branch != "HEAD");
    let origin = run_git(cwd, &["config", "--get", "remote.origin.url"])
        .await
        .ok()
        .map(|origin| origin.trim().to_string())
        .filter(|origin| !origin.is_empty());

    Some(serde_json::json!({
        "sha": sha,
        "branch": branch,
        "originUrl": origin,
    }))
}

async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, HostRuntimeError> {
    let output = run_git_output(cwd, args).await?;
    if !output.status.success() {
        return Err(HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_git_allowing_diff_exit(cwd: &Path, args: &[&str]) -> Result<String, HostRuntimeError> {
    let output = run_git_output(cwd, args).await?;
    let exit_ok = output
        .status
        .code()
        .is_some_and(|code| code == 0 || code == 1);
    if !exit_ok {
        return Err(HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_git_output(
    cwd: &Path,
    args: &[&str],
) -> Result<std::process::Output, HostRuntimeError> {
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(GIT_COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("git {} timed out", args.join(" ")),
        })?
        .map_err(|err| HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("failed to run git {}: {err}", args.join(" ")),
        })
}

async fn untracked_git_diff(cwd: &Path) -> Result<String, HostRuntimeError> {
    let output = run_git_output(cwd, &["ls-files", "--others", "--exclude-standard"]).await?;
    if !output.status.success() {
        return Ok(String::new());
    }
    let untracked = String::from_utf8_lossy(&output.stdout);
    let mut diff = String::new();
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    for file in untracked.lines().filter(|line| !line.is_empty()) {
        let output = run_git_output(
            cwd,
            &[
                "diff",
                "--no-textconv",
                "--no-ext-diff",
                "--binary",
                "--no-index",
                "--",
                null_device,
                file,
            ],
        )
        .await?;
        if output
            .status
            .code()
            .is_some_and(|code| code == 0 || code == 1)
        {
            diff.push_str(&String::from_utf8_lossy(&output.stdout));
        }
    }
    Ok(diff)
}
