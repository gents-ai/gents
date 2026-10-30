use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

use super::{CommandExecutionMode, CommandNetworkMode};
use crate::truncation::{truncate, TruncationLimits, TruncationMode};

#[derive(Serialize)]
pub(super) struct CommandOutput {
    pub(super) stdout: String,
    pub(super) stderr: String,
    #[serde(flatten)]
    pub(super) metadata: CommandMetadata,
}

#[derive(Serialize)]
pub(super) struct CommandMetadata {
    pub(super) ok: bool,
    pub(super) status: &'static str,
    pub(super) command: String,
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) exit_code: Option<i32>,
    pub(super) timed_out: bool,
    pub(super) duration_ms: u64,
    pub(super) timeout_ms: u64,
    pub(super) execution_mode: CommandExecutionMode,
    pub(super) network_mode: CommandNetworkMode,
    pub(super) sandbox: &'static str,
    pub(super) stdout_capture_incomplete: bool,
    pub(super) stderr_capture_incomplete: bool,
    pub(super) stdout_truncation: StreamTruncationMetadata,
    pub(super) stderr_truncation: StreamTruncationMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hint: Option<String>,
}

pub(super) fn foreground_timeout_hint(duration_ms: u64) -> String {
    format!(
        "timed out after {:.1}s; stdout and stderr below were captured before it was stopped. For long-running work use {} and poll its output.",
        duration_ms as f64 / 1000.0,
        crate::toolset::SPAWN_PROCESS_TOOL_NAME,
    )
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct StreamTruncationMetadata {
    returned_bytes: usize,
    total_bytes: usize,
    max_bytes: usize,
    truncated: bool,
}

pub(crate) struct TruncatedStream {
    pub(crate) content: String,
    pub(crate) shown: std::ops::Range<usize>,
    pub(super) metadata: StreamTruncationMetadata,
}

pub(crate) fn truncate_stream(text: &str, max_bytes: usize) -> TruncatedStream {
    let limits = TruncationLimits {
        max_bytes,
        max_lines: usize::MAX,
    };
    let result = truncate(text, TruncationMode::Head, &limits);
    TruncatedStream {
        content: result.text,
        shown: 0..result.returned_bytes,
        metadata: StreamTruncationMetadata {
            returned_bytes: result.returned_bytes,
            total_bytes: result.original_bytes,
            max_bytes,
            truncated: result.truncated,
        },
    }
}

pub(crate) fn stopped_stream(text: &str, max_bytes: usize) -> TruncatedStream {
    let tail = crate::tool_call_lifecycle::delivery::terminal_output_tail(text, max_bytes);
    let start = text.len() - tail.len();
    let content = if start == 0 {
        text.to_owned()
    } else {
        format!(
            "{}{tail}",
            gents_loop::truncation::tail_bytes_notice(tail.len(), text.len())
        )
    };
    TruncatedStream {
        content,
        shown: start..text.len(),
        metadata: StreamTruncationMetadata {
            returned_bytes: tail.len(),
            total_bytes: text.len(),
            max_bytes,
            truncated: start > 0,
        },
    }
}

pub(crate) fn captured_channel<'a>(
    raw: &'a str,
    stream: &'a TruncatedStream,
) -> gents_loop::live_output::CapturedChannel<'a> {
    gents_loop::live_output::CapturedChannel {
        raw,
        rendered: &stream.content,
        shown: stream.shown.clone(),
    }
}

pub(super) fn render_command_output(output: &CommandOutput, raw_json: bool) -> Result<String> {
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

pub(super) fn elapsed_ms(started: Instant) -> u64 {
    let millis = started.elapsed().as_millis();
    millis.min(u128::from(u64::MAX)) as u64
}

pub(super) fn millis(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    millis.min(u128::from(u64::MAX)) as u64
}

pub(crate) fn shell_join(argv: &[String]) -> String {
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
