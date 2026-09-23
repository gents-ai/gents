use std::hash::{Hash, Hasher};
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::{
    observe_canonical_request_output, post_graphql, request_diagnostic_hint,
    request_output_envelope, RequestOutputEnvelope,
};

use super::SubmittedRequest;

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const SPINNER_FRAMES: [char; 4] = ['-', '\\', '|', '/'];
const SPINNER_INTERVAL: Duration = Duration::from_millis(120);

#[derive(Debug, Clone)]
pub(super) struct ToolCallProgress {
    pub(super) tool_call_doc_id: String,
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) arguments: String,
    pub(super) result: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatProgressMarker {
    request_lifecycle_state: Option<RequestLifecycleState>,
    request_failure_reason: Option<String>,
    request_lease_expires_at: Option<String>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    canonical_output: Option<ChatOutputProgress>,
    tools: Vec<ChatToolProgressMarker>,
}

/// Streaming-relevant canonical output observation. Only presentation-carrying
/// variants enter change detection and the reasoning indicator; retained
/// partials are historical diagnostics and never enter either.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatOutputProgress {
    kind: &'static str,
    body_markdown: String,
    reasoning_markdown: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatToolProgressMarker {
    tool_call_key: Option<String>,
    status: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

pub(super) fn chat_progress_query(request: &SubmittedRequest) -> String {
    let request_doc_id = &request.request_doc_id;
    let scope = gents::session::session_scope_filter(
        &request.agent_did,
        &request.session_id,
        request.requester_did.as_deref(),
    );
    format!(
        r#"{{
            AgentRequest(
                filter: {{ {scope}, _docID: {{ _eq: "{request_doc_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 2
            ) {{
                _docID agent_did requester_did session_id
                request_id
                lifecycle_state
                failure_reason
                execution_generation
                execution_lease_secs
                execution_lease_expires_at
                terminal_output
                terminalized_at
                interrupt_requested_at
                valid_until
            }}
            AgentToolCall(
                filter: {{
                    {scope},
                    request_doc_id: {{ _eq: "{request_doc_id}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                tool_call_key
                tool_name
                status
                started_at
                completed_at
            }}
        }}"#,
        request_doc_id = escape_graphql_string(request_doc_id),
    )
}

pub(super) async fn load_existing_tool_call_keys(
    graphql: &str,
    session_id: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }}
            ) {{
                tool_call_key
                status
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row.get("tool_call_key")?.as_str()?.to_string(),
                row.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect())
}

/// Whether ANSI styling should be applied to CLI output: only on a real
/// terminal, and only when the caller has not opted out via `NO_COLOR`
/// (https://no-color.org — any non-empty *or* empty value disables color).
fn colors_enabled() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn dim(text: &str, colors: bool) -> String {
    if colors {
        format!("{DIM}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// A best-effort "working…" indicator on stderr while a turn is in flight
/// and nothing has streamed yet (#1622). Only appears when stdout is a
/// terminal, so piped/non-interactive output stays completely clean. It is
/// cleared — and never shown again for the rest of this turn — the moment
/// any tool activity or answer text is about to print.
struct WorkingIndicator {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl WorkingIndicator {
    fn start_if_tty() -> Option<Self> {
        if !io::stdout().is_terminal() {
            return None;
        }
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(spin(stop_rx));
        Some(Self {
            stop: Some(stop_tx),
            task: Some(task),
        })
    }

    async fn clear(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for WorkingIndicator {
    fn drop(&mut self) {
        // Best-effort cleanup for a path that never reached `clear` (e.g. an
        // early `bail!`): abort the spinner rather than await it here, since
        // `Drop` cannot be async. The process is exiting either way.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn spin(mut stop: tokio::sync::oneshot::Receiver<()>) {
    let mut ticker = tokio::time::interval(SPINNER_INTERVAL);
    let mut frame = 0usize;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                eprint!("\r{} working...", SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]);
                let _ = io::stderr().flush();
                frame += 1;
            }
            _ = &mut stop => break,
        }
    }
    // Erase the indicator line so the next thing printed starts clean.
    eprint!("\r\x1b[2K");
    let _ = io::stderr().flush();
}

/// Clears `indicator` (if it hasn't already been cleared) right before the
/// first tool event or answer text prints this turn. A no-op afterward.
async fn clear_indicator(indicator: &mut Option<WorkingIndicator>) {
    if let Some(indicator) = indicator.take() {
        indicator.clear().await;
    }
}

pub(super) async fn stream_turn_progress(
    graphql: &str,
    submitted: &SubmittedRequest,
    mut known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
    verbose: bool,
) -> Result<RequestOutputEnvelope> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut latest_progress_marker: Option<ChatProgressMarker> = None;
    let mut thinking_printed = false;
    let mut tool_output_fingerprints = std::collections::BTreeMap::new();
    let colors = colors_enabled();
    let mut indicator = WorkingIndicator::start_if_tty();

    loop {
        let query = chat_progress_query(submitted);
        let response = post_graphql(graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .context("chat request query omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1,
            "committed chat physical request missing or ambiguous"
        );
        let request_row = rows.first().cloned();
        let request = request_row
            .as_ref()
            .map(|row| {
                serde_json::from_value::<AgentRequestRow>(row.clone())
                    .context("decoding chat progress AgentRequest row")
            })
            .transpose()?;

        if let Some(request) = request.as_ref() {
            anyhow::ensure!(
                request.agent_did.as_deref() == Some(submitted.agent_did.as_str())
                    && request.requester_did == submitted.requester_did
                    && request.session_id.as_deref() == Some(submitted.session_id.as_str()),
                "chat request scope differs from committed receipt"
            );
        }

        let mut observed_output = match request.as_ref() {
            Some(request) => Some(observe_canonical_request_output(graphql, request).await?),
            None => None,
        };

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut terminal_tool_dependency_pending = false;
        for mut tool in tool_rows.iter().filter_map(decode_tool_call_progress) {
            let terminal = matches!(tool.status.as_str(), "completed" | "error");
            let presentation = crate::load_canonical_tool_presentation(
                graphql,
                &tool.tool_call_doc_id,
                submitted,
                terminal,
            )
            .await?;
            tool.arguments = presentation.arguments;
            if terminal && presentation.result.is_none() {
                terminal_tool_dependency_pending = true;
                tool.result = presentation.live_output;
                let output_fingerprint = tool.result.as_deref().map(text_fingerprint);
                let previous_fingerprint = tool_output_fingerprints
                    .get(&tool.tool_call_key)
                    .copied()
                    .flatten();
                if previous_fingerprint != output_fingerprint {
                    tool_output_fingerprints.insert(tool.tool_call_key.clone(), output_fingerprint);
                    last_progress_at = tokio::time::Instant::now();
                    clear_indicator(&mut indicator).await;
                    println!("{}", render_live_tool_line(&tool, verbose, colors));
                    io::stdout().flush()?;
                }
                continue;
            }
            tool.result = presentation.result.or(presentation.live_output);
            let output_fingerprint = tool.result.as_deref().map(text_fingerprint);
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            let previous_fingerprint = tool_output_fingerprints
                .get(&tool.tool_call_key)
                .copied()
                .flatten();
            if previous_status.as_deref() == Some(tool.status.as_str())
                && previous_fingerprint == output_fingerprint
            {
                continue;
            }
            tool_output_fingerprints.insert(tool.tool_call_key.clone(), output_fingerprint);
            known_tool_calls.insert(tool.tool_call_key.clone(), tool.status.clone());
            last_progress_at = tokio::time::Instant::now();
            clear_indicator(&mut indicator).await;
            if previous_status.is_none() && matches!(tool.status.as_str(), "completed" | "error") {
                println!("{}", render_tool_start_line(&tool, verbose, colors));
            }
            println!("{}", render_tool_line(&tool, verbose, colors));
            io::stdout().flush()?;
        }

        if terminal_tool_dependency_pending {
            if last_progress_at.elapsed() >= idle_timeout {
                anyhow::bail!(
                    "timed out waiting for canonical tool output for request {} after {}s of inactivity\n{}",
                    submitted.request_id,
                    timeout_secs,
                    request_diagnostic_hint(&submitted.request_id)
                );
            }
            tokio::time::sleep(Duration::from_secs(poll_secs)).await;
            continue;
        }

        let lifecycle_state = request.as_ref().and_then(|row| row.lifecycle_state);
        let failure_reason = request
            .as_ref()
            .and_then(|row| row.failure_reason.as_deref())
            .unwrap_or("");
        let marker = chat_progress_marker(request.as_ref(), observed_output.as_ref(), &tool_rows);
        if latest_progress_marker.as_ref() != Some(&marker) {
            latest_progress_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }
        if let Some(output) = observed_output.as_ref() {
            if !thinking_printed && should_print_thinking(lifecycle_state, output) {
                clear_indicator(&mut indicator).await;
                println!("{}", dim("[thinking]", colors));
                io::stdout().flush()?;
                thinking_printed = true;
            }
        }

        let terminal_by_request = lifecycle_state.is_some_and(RequestLifecycleState::is_terminal);
        if terminal_by_request {
            let request = request
                .as_ref()
                .context("terminal chat request lost its row before exact selection")?;
            let output = observed_output
                .take()
                .context("terminal chat request output observation is missing")?;
            match output {
                gents::session::CanonicalRequestOutput::Loading => {
                    if last_progress_at.elapsed() >= idle_timeout {
                        anyhow::bail!(
                            "timed out waiting for materialized AgentMessage {} after {}s of inactivity\n{}",
                            submitted.request_id,
                            timeout_secs,
                            request_diagnostic_hint(&submitted.request_id)
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
                gents::session::CanonicalRequestOutput::Denied => {
                    anyhow::bail!("canonical terminal output for request {} is denied", submitted.request_id)
                }
                gents::session::CanonicalRequestOutput::Conflicted => anyhow::bail!(
                    "canonical terminal output for request {} is conflicted",
                    submitted.request_id
                ),
                gents::session::CanonicalRequestOutput::Invalid => {
                    anyhow::bail!(
                        "canonical terminal output for request {} is invalid",
                        submitted.request_id
                    )
                }
                gents::session::CanonicalRequestOutput::Retracted => anyhow::bail!(
                    "canonical terminal output for request {} is retracted",
                    submitted.request_id
                ),
                gents::session::CanonicalRequestOutput::RetainedPartial(_) => anyhow::bail!(
                    "canonical terminal output for request {} resolved only to retained partial diagnostics",
                    submitted.request_id
                ),
                gents::session::CanonicalRequestOutput::TerminalNoMessage
                | gents::session::CanonicalRequestOutput::TerminalMessage { .. } => {
                    let envelope = request_output_envelope(request, output)?;
                    let presentation = match &envelope.output {
                        crate::CliOutputObservation::TerminalMessage { presentation, .. } => {
                            Some(presentation)
                        }
                        crate::CliOutputObservation::TerminalNoMessage => None,
                        _ => unreachable!("exact terminal selection produced another variant"),
                    };
                    clear_indicator(&mut indicator).await;
                    if let Some(presentation) = presentation {
                        if !presentation.body_markdown.trim().is_empty() {
                            println!("{}", presentation.body_markdown);
                            io::stdout().flush()?;
                        }
                    }

                    let error_message = failure_reason.trim();
                    if !error_message.is_empty() {
                        println!("[agent error] {error_message}");
                        println!("[inspect] gents response show {}", submitted.request_id);
                        io::stdout().flush()?;
                    } else if matches!(
                        envelope.request.lifecycle_state,
                        RequestLifecycleState::Failed | RequestLifecycleState::Dead
                    ) {
                        println!("[inspect] gents response show {}", submitted.request_id);
                        io::stdout().flush()?;
                    }

                    return Ok(envelope);
                }
                gents::session::CanonicalRequestOutput::Absent
                | gents::session::CanonicalRequestOutput::Live(_)
                | gents::session::CanonicalRequestOutput::Settling(_)
                | gents::session::CanonicalRequestOutput::Published { .. } => {
                    // Lifecycle and canonical publication replicate separately.
                    // Published is useful preview state, but only the request's
                    // exact terminal selection may settle this turn.
                    if last_progress_at.elapsed() >= idle_timeout {
                        anyhow::bail!(
                            "timed out waiting for terminal canonical selection for request {} after {}s of inactivity\n{}",
                            submitted.request_id,
                            timeout_secs,
                            request_diagnostic_hint(&submitted.request_id)
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for canonical output for request {} after {}s of inactivity\n{}",
                submitted.request_id,
                timeout_secs,
                request_diagnostic_hint(&submitted.request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

/// The one-line tool line for this poll: raw JSON args/result under
/// `--verbose`, otherwise the short, dimmed summary (#1622).
fn render_tool_line(tool: &ToolCallProgress, verbose: bool, colors: bool) -> String {
    if verbose {
        format_tool_progress_line(tool)
    } else {
        format_tool_summary_line(tool, colors)
    }
}

/// The interim line printed while a terminal tool call's canonical result
/// hasn't replicated yet and only a live/partial output snapshot is
/// available. Never reports an outcome: the call's own `status` may already
/// be "completed"/"error", but the snapshot in hand doesn't yet carry the
/// tool's self-reported `ok`/`status`, so guessing here would risk a wrong
/// "ok"/"failed" that a moment later flips when the real result lands.
fn render_live_tool_line(tool: &ToolCallProgress, verbose: bool, colors: bool) -> String {
    if verbose {
        format_live_tool_progress_line(tool)
    } else {
        format_tool_summary_line(
            &ToolCallProgress {
                status: "running".to_string(),
                ..tool.clone()
            },
            colors,
        )
    }
}

/// The catch-up "tool started" line printed when a tool call is first seen
/// already in a terminal state (its running phase was missed between polls).
fn render_tool_start_line(tool: &ToolCallProgress, verbose: bool, colors: bool) -> String {
    if verbose {
        format!(
            "[tool] {} {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments)
        )
    } else {
        format_tool_summary_line(
            &ToolCallProgress {
                status: "running".to_string(),
                result: None,
                ..tool.clone()
            },
            colors,
        )
    }
}

fn chat_output_progress(
    output: &gents::session::CanonicalRequestOutput,
) -> Option<ChatOutputProgress> {
    let (kind, presentation) = match output {
        gents::session::CanonicalRequestOutput::Live(value) => ("live", value),
        gents::session::CanonicalRequestOutput::Settling(value) => ("settling", value),
        gents::session::CanonicalRequestOutput::Published { presentation, .. } => {
            ("published", presentation)
        }
        // Retained partials are historical diagnostics, never streaming answer
        // progress; Loading/absence carry no new bytes either.
        _ => return None,
    };
    Some(ChatOutputProgress {
        kind,
        body_markdown: presentation.body_markdown.clone(),
        reasoning_markdown: presentation.reasoning_markdown.clone(),
    })
}

fn chat_progress_marker(
    request_row: Option<&AgentRequestRow>,
    output: Option<&gents::session::CanonicalRequestOutput>,
    tool_rows: &[Value],
) -> ChatProgressMarker {
    ChatProgressMarker {
        request_lifecycle_state: request_row.and_then(|row| row.lifecycle_state),
        request_failure_reason: request_row.and_then(|row| row.failure_reason.clone()),
        request_lease_expires_at: request_row
            .and_then(|row| row.execution_lease_expires_at.clone()),
        request_interrupt_requested_at: request_row
            .and_then(|row| row.interrupt_requested_at.clone()),
        request_valid_until: request_row.and_then(|row| row.valid_until.clone()),
        canonical_output: output.and_then(chat_output_progress),
        tools: tool_rows.iter().map(chat_tool_progress_marker).collect(),
    }
}

fn chat_tool_progress_marker(row: &Value) -> ChatToolProgressMarker {
    ChatToolProgressMarker {
        tool_call_key: scalar_marker(Some(row), "tool_call_key"),
        status: scalar_marker(Some(row), "status"),
        started_at: scalar_marker(Some(row), "started_at"),
        completed_at: scalar_marker(Some(row), "completed_at"),
    }
}

/// Reasoning has only begun once the request is actively processing and the
/// canonical observation carries reasoning presentation without published body
/// bytes yet.
fn should_print_thinking(
    lifecycle_state: Option<RequestLifecycleState>,
    output: &gents::session::CanonicalRequestOutput,
) -> bool {
    let (body, reasoning) = match chat_output_progress(output) {
        Some(progress) => (progress.body_markdown, progress.reasoning_markdown),
        None => return false,
    };
    lifecycle_state == Some(RequestLifecycleState::Processing)
        && body.trim().is_empty()
        && reasoning
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn scalar_marker(row: Option<&Value>, field: &str) -> Option<String> {
    let value = row?.get(field)?;
    if value.is_null() {
        return None;
    }
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_bool().map(|value| value.to_string()))
}

pub(super) fn decode_tool_call_progress(row: &Value) -> Option<ToolCallProgress> {
    Some(ToolCallProgress {
        tool_call_doc_id: row.get("_docID")?.as_str()?.to_string(),
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        arguments: String::new(),
        result: None,
    })
}

pub(super) fn format_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.status.as_str() {
        "completed" => match tool.result.as_deref().and_then(preview_compact_text) {
            Some(result) => format!(
                "[tool done] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments),
                result
            ),
            None => format!(
                "[tool done] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments)
            ),
        },
        "error" => format!(
            "[tool error] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments),
            tool.result
                .as_deref()
                .and_then(preview_compact_text)
                .unwrap_or_else(|| "-".to_string())
        ),
        _ => match tool.result.as_deref().and_then(preview_compact_text) {
            Some(output) => format!(
                "[tool] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments),
                output
            ),
            None => format!(
                "[tool] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments)
            ),
        },
    }
}

fn format_live_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.result.as_deref().and_then(preview_compact_text) {
        Some(output) => format!(
            "[tool] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments),
            output
        ),
        None => format!(
            "[tool] {} {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments)
        ),
    }
}

pub(super) fn format_tool_args_preview(value: &str) -> String {
    preview_compact_text(value)
        .map(|preview| format!("({preview})"))
        .unwrap_or_default()
}

pub(super) fn preview_compact_text(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.chars().count() > 120 {
        format!("{}...", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed.to_string()
    })
}

fn bounded_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    format!("{}...", text.chars().take(limit).collect::<String>())
}

/// The single most useful argument for a one-line tool summary: the command
/// (with its args) for a command-execution tool, the path for a file tool,
/// and best-effort otherwise. `None` when `arguments` isn't recognized JSON,
/// so callers fall back to a compact preview of the raw arguments.
fn key_argument(arguments: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let object = value.as_object()?;

    if let Some(command) = object.get("command").and_then(Value::as_str) {
        let extra_args = object
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|joined| !joined.is_empty());
        return Some(match extra_args {
            Some(extra) => format!("{command} {extra}"),
            None => command.to_string(),
        });
    }

    let path = object.get("path").and_then(Value::as_str);
    if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
        return Some(match path.filter(|path| *path != ".") {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern.to_string(),
        });
    }
    if let Some(path) = path {
        return Some(path.to_string());
    }

    None
}

/// The two prefixes gents's own tools use for their first, compact-text
/// metadata line (`gents_exec: {...}` for command tools, `gents_fs: {...}`
/// for file tools). Both carry at least `ok` (bool) and `status` (string).
const TOOL_OUTPUT_METADATA_PREFIXES: [&str; 2] = ["gents_exec: ", "gents_fs: "];

fn parse_tool_output_metadata(result: &str) -> Option<(bool, String)> {
    let prefix = TOOL_OUTPUT_METADATA_PREFIXES
        .iter()
        .find(|prefix| result.starts_with(*prefix))?;
    let rest = &result[prefix.len()..];
    let json_line = rest.split('\n').next().unwrap_or(rest);
    let value: Value = serde_json::from_str(json_line).ok()?;
    Some((
        value.get("ok").and_then(Value::as_bool).unwrap_or(true),
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    ))
}

/// One-line outcome for a terminal tool call. `None` while still running:
/// there is nothing to report yet.
enum ToolOutcome {
    Ok,
    Failed(Option<String>),
}

impl ToolOutcome {
    fn render(&self) -> String {
        match self {
            ToolOutcome::Ok => "ok".to_string(),
            ToolOutcome::Failed(Some(status)) => format!("failed: {status}"),
            ToolOutcome::Failed(None) => "failed".to_string(),
        }
    }
}

/// Derives the outcome from the tool call's own lifecycle `status` plus,
/// when present, the tool's self-reported `ok`/`status` metadata. A "completed"
/// AgentToolCall (the call ran and returned) can still report `ok: false`
/// inside its own output — e.g. a shell command that exited non-zero — and
/// that inner outcome, not the call's bookkeeping status, is what a user
/// needs to see (#1622).
fn tool_outcome(status: &str, result: Option<&str>) -> Option<ToolOutcome> {
    match status {
        "completed" => Some(
            match result.and_then(parse_tool_output_metadata) {
                Some((ok, meta_status)) if !ok => ToolOutcome::Failed(Some(meta_status)),
                _ => ToolOutcome::Ok,
            },
        ),
        "error" => Some(ToolOutcome::Failed(
            result
                .and_then(preview_compact_text)
                .map(|preview| bounded_chars(&preview, 60)),
        )),
        _ => None,
    }
}

/// Short, dimmed, one-line tool summary: tool name, its key argument, and
/// — once terminal — the outcome. Raw JSON never appears here; that's only
/// available with `--verbose` via [`format_tool_progress_line`].
pub(super) fn format_tool_summary_line(tool: &ToolCallProgress, colors: bool) -> String {
    let mut line = tool.tool_name.clone();
    let argument = key_argument(&tool.arguments).or_else(|| preview_compact_text(&tool.arguments));
    if let Some(argument) = argument {
        line.push(' ');
        line.push_str(&argument);
    }
    if let Some(outcome) = tool_outcome(&tool.status, tool.result.as_deref()) {
        line.push_str(" -> ");
        line.push_str(&outcome.render());
    }
    dim(&format!("  [tool] {line}"), colors)
}

fn text_fingerprint(value: &str) -> (usize, u64) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    (value.len(), hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_progress_query_omits_retired_tool_payload_fields() {
        let submitted = SubmittedRequest {
            request_id: "request-one".to_string(),
            session_id: "session-one".to_string(),
            agent_did: "did:key:agent".to_string(),
            behavior_id: None,
            request_doc_id: "doc-one".to_string(),
            requester_did: Some("did:key:requester".to_string()),
            input: None,
            created_at: None,
        };

        let query = chat_progress_query(&submitted);

        // Retired AgentToolCall storage fields must not be queried; the SDL
        // no longer defines them (contracts/canonical-output.md deletions).
        assert!(!query.contains(" args"));
        assert!(!query.contains(" result"));
        // Surviving tool lifecycle fields and canonical request fields remain.
        assert!(query.contains("tool_call_key"));
        assert!(query.contains("_docID"));
        assert!(query.contains("tool_name"));
        assert!(query.contains("status"));
        assert!(query.contains("started_at"));
        assert!(query.contains("completed_at"));
        assert!(query.contains("terminalized_at"));
        assert!(query.contains("terminal_output"));
    }

    #[test]
    fn canonical_tool_presentation_retains_compact_arguments_and_result() {
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "bash".into(),
            status: "completed".into(),
            arguments: "{\n  \"command\": \"printf hi\"\n}".into(),
            result: Some("hi\nthere".into()),
        };
        assert_eq!(
            format_tool_progress_line(&tool),
            "[tool done] bash ({ \"command\": \"printf hi\" }) => hi there"
        );
    }

    #[test]
    fn compact_preview_is_unicode_safe_and_bounded() {
        let preview = preview_compact_text(&"考".repeat(121)).expect("nonempty preview");
        assert_eq!(preview.chars().count(), 123);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn thinking_indicator_follows_active_lifecycle_and_projected_reasoning() {
        let reasoning_only =
            gents::session::CanonicalRequestOutput::Live(gents::session::CanonicalPresentation {
                body_markdown: String::new(),
                reasoning_markdown: Some("pondering the request".to_string()),
                selected_source: None,
            });
        assert!(should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &reasoning_only
        ));

        // Inference has not begun during claimed work; no reasoning yet.
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Claimed),
            &reasoning_only
        ));

        let with_content =
            gents::session::CanonicalRequestOutput::Live(gents::session::CanonicalPresentation {
                body_markdown: "answer body".to_string(),
                reasoning_markdown: Some("pondering the request".to_string()),
                selected_source: None,
            });
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &with_content
        ));

        let no_reasoning =
            gents::session::CanonicalRequestOutput::Live(gents::session::CanonicalPresentation {
                body_markdown: String::new(),
                reasoning_markdown: None,
                selected_source: None,
            });
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &no_reasoning
        ));
        // Loading/absence carry no presentation bytes at all.
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &gents::session::CanonicalRequestOutput::Loading
        ));
        assert!(!should_print_thinking(None, &reasoning_only));
    }

    #[test]
    fn progress_marker_ignores_retained_partial_diagnostics() {
        let absent = chat_progress_marker(
            None,
            Some(&gents::session::CanonicalRequestOutput::Absent),
            &[],
        );
        let retained = chat_progress_marker(
            None,
            Some(&gents::session::CanonicalRequestOutput::RetainedPartial(
                Vec::new(),
            )),
            &[],
        );

        // Retained partials are historical diagnostics, not streaming answer
        // progress, so they must not register new output bytes.
        assert_eq!(absent.canonical_output, retained.canonical_output);
        assert_eq!(retained.canonical_output, None);
    }

    fn bash_tool(status: &str, result: Option<&str>) -> ToolCallProgress {
        ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "bash".into(),
            status: status.into(),
            arguments: r#"{"args":["branch","--show-current"],"command":"git"}"#.into(),
            result: result.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn key_argument_joins_command_and_args_for_bash() {
        assert_eq!(
            key_argument(r#"{"args":["branch","--show-current"],"command":"git"}"#),
            Some("git branch --show-current".to_string())
        );
        assert_eq!(
            key_argument(r#"{"command":"ls"}"#),
            Some("ls".to_string())
        );
    }

    #[test]
    fn key_argument_uses_path_for_file_tools() {
        assert_eq!(
            key_argument(r#"{"path":"notes.txt","start_line":1}"#),
            Some("notes.txt".to_string())
        );
    }

    #[test]
    fn key_argument_prefers_pattern_with_non_default_path_for_search_tools() {
        assert_eq!(
            key_argument(r#"{"pattern":"TODO","path":"crates/gents"}"#),
            Some("TODO in crates/gents".to_string())
        );
        assert_eq!(
            key_argument(r#"{"pattern":"TODO","path":"."}"#),
            Some("TODO".to_string())
        );
    }

    #[test]
    fn key_argument_is_none_for_unrecognized_shape() {
        assert_eq!(key_argument(r#"{"objective":"ship it"}"#), None);
        assert_eq!(key_argument("not json"), None);
    }

    #[test]
    fn tool_summary_line_reports_ok_for_a_successful_completed_bash_call() {
        let tool = bash_tool(
            "completed",
            Some(
                "gents_exec: {\"ok\":true,\"status\":\"success\",\"exit_code\":0}\nstdout:\n(empty)\nstderr:\n(empty)",
            ),
        );
        assert_eq!(
            format_tool_summary_line(&tool, false),
            "  [tool] bash git branch --show-current -> ok"
        );
    }

    #[test]
    fn tool_summary_line_reports_failed_with_short_status_for_a_nonzero_exit() {
        // This is the exact scenario the raw-JSON transcript in #1622 showed:
        // the AgentToolCall itself completed (the command ran and returned),
        // but its own output says the command failed.
        let tool = bash_tool(
            "completed",
            Some(
                "gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\",\"exit_code\":1}\nstdout:\n(empty)\nstderr:\nfatal: not a git repository",
            ),
        );
        assert_eq!(
            format_tool_summary_line(&tool, false),
            "  [tool] bash git branch --show-current -> failed: exit_nonzero"
        );
    }

    #[test]
    fn tool_summary_line_reports_failed_for_an_infra_level_tool_error() {
        let tool = bash_tool("error", Some("command denied by policy: rm"));
        assert_eq!(
            format_tool_summary_line(&tool, false),
            "  [tool] bash git branch --show-current -> failed: command denied by policy: rm"
        );
    }

    #[test]
    fn tool_summary_line_has_no_outcome_while_still_running() {
        let tool = bash_tool("running", None);
        assert_eq!(
            format_tool_summary_line(&tool, false),
            "  [tool] bash git branch --show-current"
        );
    }

    #[test]
    fn tool_summary_line_uses_path_for_a_successful_file_read() {
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "read_file".into(),
            status: "completed".into(),
            arguments: r#"{"path":"notes.txt"}"#.into(),
            result: Some(
                "gents_fs: {\"ok\":true,\"status\":\"success\",\"tool\":\"read_file\",\"path\":\"notes.txt\"}\nchat-tool-token"
                    .into(),
            ),
        };
        assert_eq!(
            format_tool_summary_line(&tool, false),
            "  [tool] read_file notes.txt -> ok"
        );
    }

    #[test]
    fn tool_summary_line_falls_back_to_a_compact_preview_for_unknown_shapes() {
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "create_goal".into(),
            status: "completed".into(),
            arguments: r#"{"objective":"ship it","token_budget":100}"#.into(),
            result: None,
        };
        assert_eq!(
            format_tool_summary_line(&tool, false),
            r#"  [tool] create_goal {"objective":"ship it","token_budget":100} -> ok"#
        );
    }

    #[test]
    fn tool_summary_line_is_dimmed_only_when_colors_are_enabled() {
        let tool = bash_tool("running", None);
        let colored = format_tool_summary_line(&tool, true);
        assert!(colored.starts_with(DIM));
        assert!(colored.ends_with(RESET));
        assert!(colored.contains("[tool] bash git branch --show-current"));

        let plain = format_tool_summary_line(&tool, false);
        assert!(!plain.contains('\x1b'));
        assert_eq!(plain, "  [tool] bash git branch --show-current");
    }

    #[test]
    fn verbose_tool_line_is_unaffected_by_the_short_summary_formatter() {
        let tool = bash_tool(
            "completed",
            Some("gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\"}"),
        );
        // `--verbose` still gets the original raw-JSON rendering, unchanged.
        assert_eq!(
            render_tool_line(&tool, true, false),
            format_tool_progress_line(&tool)
        );
        assert!(render_tool_line(&tool, true, false).contains("exit_nonzero"));
    }
}
