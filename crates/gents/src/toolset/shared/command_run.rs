use std::time::Duration;

use anyhow::{anyhow, Context};
use gents_loop::live_output::COMMAND_OUTPUT_META_PREFIX as OUTPUT_META_PREFIX;

use super::super::context::{ToolContext, ToolError};
use super::command_launch::{command_guard_error, prepare_command_launch};
use super::command_output::{
    captured_channel, elapsed_ms, foreground_timeout_hint, millis, render_command_output,
    shell_join, stopped_stream, truncate_stream, CommandMetadata, CommandOutput,
};
use super::command_validation::validate_command_policy;
use super::{effective_command_policy, CommandExecutionPolicy};
use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
use crate::tool_call_lifecycle::runtime::tool_execution_bounds;
use crate::tool_call_lifecycle::FailureClass;

#[expect(
    clippy::too_many_arguments,
    reason = "run_command_for_test is called by tests outside the toolset module"
)]
pub(crate) async fn run_command(
    context: &ToolContext,
    tool_name: &'static str,
    command_name: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
    policy: &CommandExecutionPolicy,
    raw_json: bool,
    max_output_chars: usize,
) -> std::result::Result<String, ToolError> {
    let policy = effective_command_policy(policy);
    validate_command_policy(command_name, args, &policy)?;
    let cwd = context.resolve_existing_dir(cwd)?;
    let argv = std::iter::once(command_name.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let command_line = shell_join(&argv);
    let root = context.root();
    let launch = prepare_command_launch(&root, command_name, args, &policy).await?;
    crate::plugin::approval::workspace::authorize_command(
        &argv,
        &cwd,
        &root,
        launch.writable_root.as_deref(),
        &policy,
    )
    .await
    .map_err(|error| {
        ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
    })?;
    launch.revalidate().await.map_err(command_guard_error)?;
    let bounds = tool_execution_bounds(timeout);
    let live_output = bounds.live_output.clone();
    let request_deadline = bounds.request_deadline_at;
    let started = bounds.armed_at;
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: std::iter::once(launch.program)
            .chain(launch.args)
            .collect::<Vec<_>>(),
        cwd: cwd.clone(),
        deadline_at: bounds.deadline_at,
        cancellation_token: bounds.cancellation_token,
        max_output_bytes: usize::MAX,
        stdin: Vec::new(),
        environment: Some(launch.environment),
        tool_name: Some(tool_name.to_string()),
        live_output: live_output.clone(),
    })
    .await;
    let duration_ms = elapsed_ms(started);
    let (
        exit_code,
        timed_out,
        stdout_bytes,
        stderr_bytes,
        stdout_capture_incomplete,
        stderr_capture_incomplete,
    ) = match outcome {
        ManagedExecOutcome::Exited {
            code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => (
            code,
            false,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        ),
        ManagedExecOutcome::TimedOut {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            ..
        } => {
            if request_deadline.is_some_and(|deadline| chrono::Utc::now() >= deadline) {
                return Ok("command was stopped at the request deadline".to_string());
            }
            (
                None,
                true,
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
            )
        }
        ManagedExecOutcome::Cancelled { .. } => return Ok("command was cancelled".to_string()),
        ManagedExecOutcome::SpawnFailed { error } => {
            return Err(anyhow!("spawning managed command failed: {error}").into());
        }
    };
    let stdout_raw = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr_raw = String::from_utf8_lossy(&stderr_bytes).into_owned();

    let channel = if timed_out {
        stopped_stream
    } else {
        truncate_stream
    };
    let stdout = channel(&stdout_raw, max_output_chars);
    let stderr = channel(&stderr_raw, max_output_chars);
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
        network_mode: policy.effective_network_mode(),
        sandbox: launch.sandbox,
        stdout_capture_incomplete,
        stderr_capture_incomplete,
        stdout_truncation: stdout.metadata,
        stderr_truncation: stderr.metadata,
        hint: (timed_out
            && !crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
                .is_some_and(|context| context.background))
        .then(|| foreground_timeout_hint(duration_ms)),
    };
    let output = CommandOutput {
        metadata,
        stdout: stdout.content.clone(),
        stderr: stderr.content.clone(),
    };
    let rendered = render_command_output(&output, raw_json).map_err(ToolError::from)?;
    if let Some(writer) = live_output {
        let metadata_json = serde_json::to_string(&output.metadata)
            .context("serializing command metadata for canonical presentation")
            .map_err(ToolError::from)?;
        let head = format!("{OUTPUT_META_PREFIX}{metadata_json}\n");
        let layout = if raw_json {
            gents_loop::live_output::CommandPresentationLayout::JsonObject {
                metadata_json: &metadata_json,
            }
        } else {
            gents_loop::live_output::CommandPresentationLayout::Labeled {
                head: &head,
                json_string: false,
            }
        };
        writer
            .prepare_command_presentation(
                &rendered,
                layout,
                captured_channel(&stdout_raw, &stdout),
                captured_channel(&stderr_raw, &stderr),
            )
            .await
            .map_err(|error| ToolError::from(anyhow::Error::from(error)))?;
    }
    if output.metadata.ok {
        Ok(rendered)
    } else {
        let class = if output.metadata.timed_out {
            FailureClass::External
        } else {
            FailureClass::ToolReturnedError
        };
        Err(ToolError::reported_failure(class, rendered))
    }
}
