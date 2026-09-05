use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Context as _};
use gents_fs_runner::protocol::{NativeFsRunnerRequest, NativeFsRunnerResponse};

use super::shared::{ToolContext, ToolError};
use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
use crate::tool_call_lifecycle::runtime::current_tool_runtime_context;
use crate::tool_call_lifecycle::FailureClass;

const MAX_NATIVE_RUNNER_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const RUNNER_ENV: &str = "GENTS_FS_RUNNER";
const MAX_FS_RUNNER_SECONDS: i64 = 120;
static SELF_RUNNER_ENABLED: AtomicBool = AtomicBool::new(false);

/// Declare that the current host executable implements the hidden
/// `__native-fs-runner` command.
pub fn enable_self_runner() {
    SELF_RUNNER_ENABLED.store(true, Ordering::Relaxed);
}

fn effective_deadline(
    request_deadline: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    let cap = now + chrono::Duration::seconds(MAX_FS_RUNNER_SECONDS);
    request_deadline.map_or(cap, |deadline| deadline.min(cap))
}

/// Text for a managed-exec timeout. When the REQUEST deadline is what
/// expired, this text never reaches the model: the dispatcher's envelope
/// shares the same deadline and resolves an elapsed deadline to
/// `ToolOutcome::TimedOut`, including when this inner managed boundary wakes
/// the dispatcher before its sibling deadline sleep. A per-call cap expiry,
/// by contrast, is an ordinary model-actionable tool result.
fn fs_runner_timed_out_result(
    tool_name: &str,
    request_deadline: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if request_deadline.is_some_and(|deadline| now >= deadline) {
        return format!(
            "native filesystem runner for {tool_name} was stopped at the request deadline"
        );
    }
    format!(
        "native filesystem runner for {tool_name} exceeded the {MAX_FS_RUNNER_SECONDS}s per-call cap and was stopped before completing. Narrow the path or pattern (a more specific anchor prunes the walk), or split the search into smaller calls."
    )
}

fn fs_runner_timed_out_error(
    tool_name: &str,
    request_deadline: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> ToolError {
    ToolError::reported_failure(
        FailureClass::External,
        fs_runner_timed_out_result(tool_name, request_deadline, now),
    )
}

#[derive(Clone)]
pub(crate) struct NativeFsRunner {
    root: PathBuf,
    base: PathBuf,
}

impl NativeFsRunner {
    pub(crate) fn new(context: &ToolContext) -> Self {
        Self {
            root: context.root(),
            base: context.base(),
        }
    }

    /// Typed glob helper for LSP diagnostics. Returns resolved paths, not
    /// model-facing GlobTool text. `raw_json` is an internal decode seam.
    pub(crate) async fn glob_paths(
        &self,
        pattern: &str,
        max_matches: usize,
    ) -> Result<Vec<PathBuf>, ToolError> {
        // The runner renders matches relative to its effective request base,
        // which may be a workspace_cwd below the tool root. Capture that same
        // base for decoding instead of incorrectly rebasing paths at root.
        let base = self.effective_base();
        let raw = self
            .run(
                NativeFsRunnerRequest::Glob(gents_fs_runner::protocol::GlobArgs {
                    pattern: pattern.to_string(),
                    path: None,
                    max_matches,
                    raw_json: true,
                    max_entries_visited: None,
                    max_wall_ms: None,
                }),
                "lsp",
            )
            .await?;
        Ok(parse_glob_match_paths(&raw, &base))
    }

    pub(super) async fn run(
        &self,
        request: NativeFsRunnerRequest,
        tool_name: &'static str,
    ) -> Result<String, ToolError> {
        let runner = resolve_runner_command(&self.effective_root(), &self.effective_base())?;
        self.run_resolved(request, tool_name, runner).await
    }

    async fn run_resolved(
        &self,
        request: NativeFsRunnerRequest,
        tool_name: &'static str,
        runner: RunnerCommand,
    ) -> Result<String, ToolError> {
        let runtime = current_tool_runtime_context();
        let request_deadline = runtime.as_ref().and_then(|runtime| runtime.deadline_at);
        let deadline_at = Some(effective_deadline(request_deadline, chrono::Utc::now()));
        let cancellation_token = runtime
            .as_ref()
            .map(|runtime| runtime.cancellation_token.clone())
            .unwrap_or_default();
        let live_output = runtime
            .as_ref()
            .and_then(|runtime| runtime.live_output.clone());
        let root = self.effective_root();
        let (argv, environment) = if runtime
            .as_ref()
            .is_some_and(|scope| scope.workspace_artifact.is_some())
        {
            // The native helper remains the same read-only typed protocol. In
            // artifact scope its process must use the shared grant validation,
            // sandbox and derived environment; never an unsandboxed fallback.
            let constraints = super::CommandConstraints {
                allowed_argv_prefixes: Vec::new(),
                forbidden_argv_prefixes: Vec::new(),
                network_mode: super::CommandNetworkMode::Disabled,
                execution_mode: super::CommandExecutionMode::ArtifactWrite,
                sandbox: super::CommandExecutionMode::ArtifactWrite,
                deny_all_argv: false,
                deny_git_metadata_writes: true,
            };
            let (program, args, environment, _) = super::prepare_managed_command(
                &root,
                &runner.argv[0],
                &runner.argv[1..],
                &constraints,
            )
            .await?;
            let mut argv = vec![program.to_string_lossy().into_owned()];
            argv.extend(args);
            (argv, Some(environment))
        } else {
            (runner.argv, None)
        };
        let stdin = serde_json::to_vec(&request)
            .with_context(|| format!("serializing native filesystem request for {tool_name}"))?;

        match run_managed_exec(ManagedExecRequest {
            argv,
            cwd: runner.cwd,
            deadline_at,
            cancellation_token,
            max_output_bytes: MAX_NATIVE_RUNNER_OUTPUT_BYTES,
            stdin,
            environment,
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
            ManagedExecOutcome::TimedOut { .. } => Err(fs_runner_timed_out_error(
                tool_name,
                request_deadline,
                chrono::Utc::now(),
            )),
            // Never model-facing: the dispatcher's envelope polls its (fired)
            // cancellation branch first and resolves to `ToolOutcome::Cancelled`.
            ManagedExecOutcome::Cancelled { .. } => Ok(format!(
                "native filesystem runner for {tool_name} was cancelled"
            )),
            ManagedExecOutcome::SpawnFailed { error } => Err(anyhow!(
                "native filesystem runner for {tool_name} failed to spawn: {error}"
            )
            .into()),
        }
    }

    fn effective_root(&self) -> PathBuf {
        current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_root)
            .unwrap_or_else(|| self.root.clone())
    }

    fn effective_base(&self) -> PathBuf {
        let root = self.effective_root();
        let runtime_base = current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_cwd)
            .and_then(|base| resolve_base_dir(&root, &base).ok());
        runtime_base.unwrap_or_else(|| resolve_base_dir(&root, &self.base).unwrap_or(root))
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
        "native filesystem runner binary not found; set {RUNNER_ENV}, install gents-fs-runner next to the gents binary, or run a gents binary with the built-in native filesystem runner"
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
        "gents-fs-runner.exe"
    } else {
        "gents-fs-runner"
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
    if stem == "gents" || SELF_RUNNER_ENABLED.load(Ordering::Relaxed) {
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

pub(crate) fn parse_glob_match_paths(raw: &str, base: &Path) -> Vec<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
    value
        .get("matches")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("path").and_then(serde_json::Value::as_str))
                .map(|rel| {
                    let candidate = PathBuf::from(rel);
                    if candidate.is_absolute() {
                        candidate
                    } else {
                        base.join(rel)
                    }
                })
                .collect()
        })
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn artifact_native_runner_uses_shared_sandbox_and_rejects_stale_grant() {
        let mut fixture = crate::workspace::artifact_test_fixture(&[]).await;
        let source = fixture.grant.source_root().to_path_buf();
        let original = std::fs::read(source.join("README.md")).unwrap();
        let context = ToolContext::new(source.clone(), false).unwrap();
        let runner = NativeFsRunner::new(&context);
        let scope = crate::tool_call_lifecycle::runtime::ToolWorkspaceScope {
            workspace_cwd: Some(source.clone()),
            workspace_root: Some(source.clone()),
            workspace_authority: Some(crate::toolset::WorkspaceAuthority::ReadOnly),
            workspace_artifact: Some(fixture.grant.clone()),
        };
        let request = || {
            NativeFsRunnerRequest::ListFiles(gents_fs_runner::protocol::ListFilesArgs {
                path: None,
                recursive: false,
                max_entries: 10,
                raw_json: false,
                max_entries_visited: None,
                max_wall_ms: None,
            })
        };
        let command = || RunnerCommand {
            // Exercise the actual resolved-runner launch owner without changing
            // process-wide GENTS_FS_RUNNER or requiring an adjacent binary.
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                r#"cat README.md > "$CARGO_TARGET_DIR/native-readback" || exit 31
if printf changed > README.md; then exit 32; fi
printf '{"ok":true,"output":"sandboxed read","error":null}'"#
                    .into(),
            ],
            cwd: source.clone(),
        };
        let output = crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_workspace_overlay(
            None, tokio_util::sync::CancellationToken::new(), scope.clone(),
            None, None, None, Default::default(), false,
            runner.run_resolved(request(), "glob", command()),
        ).await.unwrap();
        assert_eq!(output, "sandboxed read");
        assert_eq!(std::fs::read(source.join("README.md")).unwrap(), original);
        assert_eq!(
            std::fs::read(fixture.grant.root().join("target/native-readback")).unwrap(),
            original
        );
        std::fs::remove_file(fixture.grant.root().join("target/native-readback")).unwrap();
        fixture
            .owner
            .terminalize_owned_without_stream(
                crate::lifecycle::RequestTerminalOutcome::Failed,
                Some("end reader incarnation"),
            )
            .await
            .unwrap();
        let error = crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_workspace_overlay(
            None, tokio_util::sync::CancellationToken::new(), scope,
            None, None, None, Default::default(), false,
            runner.run_resolved(request(), "glob", command()),
        ).await.unwrap_err();
        assert!(!error.to_string().is_empty());
        assert!(!fixture.grant.root().join("target/native-readback").exists());
        assert_eq!(std::fs::read(source.join("README.md")).unwrap(), original);
    }

    #[test]
    fn effective_deadline_caps_long_request_deadlines() {
        let now = Utc::now();
        let request_deadline = now + ChronoDuration::hours(12);
        let effective = effective_deadline(Some(request_deadline), now);
        assert_eq!(
            effective,
            now + ChronoDuration::seconds(MAX_FS_RUNNER_SECONDS)
        );
    }

    #[test]
    fn effective_deadline_keeps_shorter_request_deadlines() {
        let now = Utc::now();
        let request_deadline = now + ChronoDuration::seconds(30);
        assert_eq!(
            effective_deadline(Some(request_deadline), now),
            request_deadline
        );
    }

    #[test]
    fn effective_deadline_bounds_calls_without_request_deadline() {
        let now = Utc::now();
        assert_eq!(
            effective_deadline(None, now),
            now + ChronoDuration::seconds(MAX_FS_RUNNER_SECONDS)
        );
    }

    // The per-call cap must NOT read as a request-deadline expiry: the
    // dispatcher's envelope converts a genuine request-deadline expiry to
    // `ToolOutcome::TimedOut` (terminating the request), while a cap expiry
    // is an ordinary, model-actionable tool result.
    #[test]
    fn cap_expiry_before_request_deadline_is_an_ordinary_tool_result() {
        let now = Utc::now();
        let result = fs_runner_timed_out_result("grep", Some(now + ChronoDuration::hours(12)), now);
        assert!(result.contains("per-call cap"), "{result}");
    }

    #[test]
    fn cap_expiry_without_request_deadline_is_an_ordinary_tool_result() {
        let now = Utc::now();
        let result = fs_runner_timed_out_result("grep", None, now);
        assert!(result.contains("per-call cap"), "{result}");
    }

    #[test]
    fn cap_expiry_is_a_recoverable_typed_failure() {
        let now = Utc::now();
        let dispatch_error = fs_runner_timed_out_error("grep", None, now).into_dispatch_error();
        let outcome =
            crate::tool_call_lifecycle::ToolOutcome::from_dispatch("grep", Err(dispatch_error));
        assert!(matches!(
            outcome,
            crate::tool_call_lifecycle::ToolOutcome::Failed {
                class: FailureClass::External,
                ..
            }
        ));
    }

    #[test]
    fn expired_request_deadline_yields_deadline_text() {
        let now = Utc::now();
        let deadline = now - ChronoDuration::seconds(1);
        let result = fs_runner_timed_out_result("grep", Some(deadline), now);
        assert!(result.contains("request deadline"), "{result}");
    }
}
