use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Git is an external process; dropping a timed-out command kills the child, so an
/// unresponsive repository holds up nothing past this.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// The working tree's changes since its upstream's merge base, or `HEAD` without one, with
/// each untracked file as an add.
pub(super) async fn diff_to_remote(cwd: &Path) -> Result<String, String> {
    run_git(cwd, &["rev-parse", "--show-toplevel"]).await?;
    let base = match run_git(cwd, &["merge-base", "HEAD", "@{upstream}"]).await {
        Ok(sha) => sha,
        Err(_) => run_git(cwd, &["rev-parse", "HEAD"]).await?,
    };
    let base = base.trim();
    if base.is_empty() {
        return Err(format!(
            "failed to compute git base sha for {}",
            cwd.display()
        ));
    }
    let mut diff =
        run_git_allowing_diff_exit(cwd, &["diff", "--no-textconv", "--no-ext-diff", base]).await?;
    diff.push_str(&untracked_diff(cwd).await?);
    Ok(diff)
}

/// The checked-out branch, none on a detached `HEAD` or outside a repository.
pub(super) async fn branch(cwd: &Path) -> Option<String> {
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()?;
    let branch = branch.trim();
    (!branch.is_empty() && branch != "HEAD").then(|| branch.to_string())
}

async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_output(cwd, args).await?;
    if !output.status.success() {
        return Err(failed(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git diff` exits 1 when there are differences.
async fn run_git_allowing_diff_exit(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_output(cwd, args).await?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(failed(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn failed(args: &[&str], output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("git {} failed: {}", args.join(" "), stderr.trim())
}

async fn run_git_output(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new("git");
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
    let output = gents::exec::native_spawn::output_tokio(&mut command);
    tokio::time::timeout(GIT_COMMAND_TIMEOUT, output)
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|error| format!("failed to run git {}: {error}", args.join(" ")))
}

async fn untracked_diff(cwd: &Path) -> Result<String, String> {
    let output = run_git_output(cwd, &["ls-files", "--others", "--exclude-standard"]).await?;
    if !output.status.success() {
        return Ok(String::new());
    }
    let untracked = String::from_utf8_lossy(&output.stdout);
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut diff = String::new();
    for file in untracked.lines().filter(|line| !line.is_empty()) {
        let args = [
            "diff",
            "--no-textconv",
            "--no-ext-diff",
            "--binary",
            "--no-index",
            "--",
            null_device,
            file,
        ];
        let output = run_git_output(cwd, &args).await?;
        if matches!(output.status.code(), Some(0 | 1)) {
            diff.push_str(&String::from_utf8_lossy(&output.stdout));
        }
    }
    Ok(diff)
}
