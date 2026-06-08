use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use defra_native_fs_runner::protocol::{NativeFsRunnerRequest, NativeFsRunnerResponse};
use tokio_util::sync::CancellationToken;

use super::shared::{ToolContext, ToolError};
use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
use crate::tool_call_lifecycle::runtime::{
    cancelled_result, current_tool_runtime_context, timeout_result,
};

const MAX_NATIVE_RUNNER_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const RUNNER_ENV: &str = "DEFRA_NATIVE_FS_RUNNER";

#[derive(Clone)]
pub(super) struct NativeFsRunner {
    root: PathBuf,
    base: PathBuf,
}

impl NativeFsRunner {
    pub(super) fn new(context: &ToolContext) -> Self {
        Self {
            root: context.root().to_path_buf(),
            base: context.base().to_path_buf(),
        }
    }

    pub(super) async fn run(
        &self,
        request: NativeFsRunnerRequest,
        tool_name: &'static str,
    ) -> Result<String, ToolError> {
        let runtime = current_tool_runtime_context();
        let deadline_at = runtime.as_ref().and_then(|runtime| runtime.deadline_at);
        let cancellation_token = runtime
            .as_ref()
            .map(|runtime| runtime.cancellation_token.clone())
            .unwrap_or_else(CancellationToken::new);
        let live_output = runtime
            .as_ref()
            .and_then(|runtime| runtime.live_output.clone());
        let base = self.effective_base();
        let runner = resolve_runner_command(&self.root, &base)?;
        let stdin = serde_json::to_vec(&request)
            .with_context(|| format!("serializing native filesystem request for {tool_name}"))?;

        match run_managed_exec(ManagedExecRequest {
            argv: runner.argv,
            cwd: runner.cwd,
            deadline_at,
            cancellation_token,
            max_output_bytes: MAX_NATIVE_RUNNER_OUTPUT_BYTES,
            stdin,
            tool_name: Some(tool_name.to_string()),
            live_output,
        })
        .await
        {
            ManagedExecOutcome::Exited {
                code,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
            } => handle_exited(
                tool_name,
                code,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
            ),
            ManagedExecOutcome::TimedOut { .. } => Ok(timeout_result(deadline_at)),
            ManagedExecOutcome::Cancelled { .. } => Ok(cancelled_result()),
            ManagedExecOutcome::SpawnFailed { error } => Err(anyhow!(
                "native filesystem runner for {tool_name} failed to spawn: {error}"
            )
            .into()),
        }
    }

    fn effective_base(&self) -> PathBuf {
        let runtime_base = current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_cwd)
            .and_then(|base| resolve_base_dir(&self.root, &base).ok());
        runtime_base.unwrap_or_else(|| self.base.clone())
    }
}

struct RunnerCommand {
    argv: Vec<String>,
    cwd: PathBuf,
}

fn resolve_runner_command(root: &Path, base: &Path) -> Result<RunnerCommand, ToolError> {
    if let Ok(path) = std::env::var(RUNNER_ENV) {
        if !path.trim().is_empty() {
            return Ok(RunnerCommand {
                argv: runner_argv(path, root, base),
                cwd: base.to_path_buf(),
            });
        }
    }

    if let Some(candidate) = self_runner_binary() {
        return Ok(RunnerCommand {
            argv: self_runner_argv(candidate.to_string_lossy().into_owned(), root, base),
            cwd: base.to_path_buf(),
        });
    }

    if let Some(candidate) = adjacent_runner_binary() {
        return Ok(RunnerCommand {
            argv: runner_argv(candidate.to_string_lossy().into_owned(), root, base),
            cwd: base.to_path_buf(),
        });
    }

    Err(anyhow!(
        "native filesystem runner binary not found; set {RUNNER_ENV}, install defra-native-fs-runner next to the defra-agent binary, or run a defra-agent binary with the built-in native filesystem runner"
    )
    .into())
}

fn runner_argv(program: String, root: &Path, base: &Path) -> Vec<String> {
    let mut argv = vec![
        program,
        "--root".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    if base != root {
        argv.push("--base".to_string());
        argv.push(base.to_string_lossy().into_owned());
    }
    argv
}

fn self_runner_argv(program: String, root: &Path, base: &Path) -> Vec<String> {
    let mut argv = vec![
        program,
        "__native-fs-runner".to_string(),
        "--root".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    if base != root {
        argv.push("--base".to_string());
        argv.push(base.to_string_lossy().into_owned());
    }
    argv
}

fn resolve_base_dir(root: &Path, base: &Path) -> Result<PathBuf, ToolError> {
    let canonical = std::fs::canonicalize(base)
        .with_context(|| format!("canonicalizing native filesystem base {}", base.display()))?;
    if canonical.is_dir() && canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(anyhow!(
            "native filesystem base {} is outside root {} or is not a directory",
            canonical.display(),
            root.display()
        )
        .into())
    }
}

fn adjacent_runner_binary() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "defra-native-fs-runner.exe"
    } else {
        "defra-native-fs-runner"
    };

    let current = std::env::current_exe().ok()?;
    let mut dirs = Vec::new();
    if let Some(parent) = current.parent() {
        dirs.push(parent.to_path_buf());
        if let Some(grandparent) = parent.parent() {
            dirs.push(grandparent.to_path_buf());
        }
    }

    dirs.into_iter()
        .map(|dir| dir.join(exe_name))
        .find(|candidate| candidate.is_file())
}

fn self_runner_binary() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let stem = current.file_stem()?.to_str()?;
    if stem == "defra-agent" {
        Some(current)
    } else {
        None
    }
}

fn handle_exited(
    tool_name: &'static str,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
) -> Result<String, ToolError> {
    let response = decode_runner_response(&stdout);
    if code == Some(0) {
        let response = response?;
        if response.ok {
            return response.output.ok_or_else(|| {
                anyhow!("native filesystem runner for {tool_name} returned no output").into()
            });
        }
        return Err(anyhow!(
            "native filesystem runner for {tool_name} returned an error: {}",
            response
                .error
                .unwrap_or_else(|| "unknown error".to_string())
        )
        .into());
    }

    if let Ok(response) = response {
        if let Some(error) = response.error {
            return Err(anyhow!(
                "native filesystem runner for {tool_name} exited with {:?}: {error}",
                code
            )
            .into());
        }
    }

    let stderr = String::from_utf8_lossy(&stderr);
    let stdout_preview = String::from_utf8_lossy(&stdout);
    Err(anyhow!(
        "native filesystem runner for {tool_name} exited with {:?}; stderr_truncated={stderr_truncated}; stdout_truncated={stdout_truncated}; stderr={}; stdout={}",
        code,
        truncate_error_preview(&stderr),
        truncate_error_preview(&stdout_preview)
    )
    .into())
}

fn decode_runner_response(stdout: &[u8]) -> Result<NativeFsRunnerResponse, ToolError> {
    serde_json::from_slice(stdout)
        .context("decoding native filesystem runner response")
        .map_err(Into::into)
}

fn truncate_error_preview(text: &str) -> String {
    const MAX_ERROR_PREVIEW_CHARS: usize = 1_000;
    if text.chars().count() <= MAX_ERROR_PREVIEW_CHARS {
        return text.to_string();
    }
    format!(
        "{}... [truncated]",
        text.chars()
            .take(MAX_ERROR_PREVIEW_CHARS)
            .collect::<String>()
    )
}
