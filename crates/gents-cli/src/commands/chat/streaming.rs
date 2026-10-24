use gents::config_client::GraphqlEndpoint;
use std::hash::{Hash, Hasher};
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use anyhow::Result;
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

/// The exact `_docID` request read carries no `order`: DefraDB plans an
/// ordered read as an index scan and applies `limit` before the `_docID`
/// filter, so later rows in the collection would hide this request.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StreamError {
    #[error(transparent)]
    PluginApproval(#[from] gents::plugin::approval::PluginApprovalError),
    #[error("chat request query omitted rows")]
    QueryOmittedRows,
    #[error("committed chat physical request missing or ambiguous")]
    RequestRowMissing,
    #[error("decoding chat progress AgentRequest row")]
    DecodeRequestRow(#[source] serde_json::Error),
    #[error("chat request scope differs from committed receipt")]
    RequestScopeMismatch,
    #[error("terminal chat request lost its row before exact selection")]
    TerminalRowLost,
    #[error("terminal chat request output observation is missing")]
    TerminalOutputMissing,
    #[error("timed out waiting for {wait} after {timeout_secs}s of inactivity\n{hint}")]
    IdleTimeout {
        wait: String,
        timeout_secs: u64,
        hint: String,
    },
    #[error("canonical terminal output for request {request_id} is {state}")]
    TerminalOutputUnusable {
        request_id: String,
        state: &'static str,
    },
    #[error("canonical terminal output for request {request_id} resolved only to retained partial diagnostics")]
    RetainedPartialOnly { request_id: String },
}

pub(super) fn chat_progress_query(request: &SubmittedRequest) -> String {
    let request_doc_id = &request.request_doc_id;
    let scope = gents::session::session_scope_filter(
        &request.node_did,
        &request.session_id,
        request.requester_did.as_deref(),
    );
    format!(
        r#"{{
            AgentRequest(
                filter: {{ {scope}, _docID: {{ _eq: "{request_doc_id}" }} }},
                limit: 2
            ) {{
                _docID node_did requester_did session_id
                request_id agent_id
                lifecycle_state
                failure_reason
                superseded_by_request
                superseded_by_request_doc_id
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

pub(crate) async fn load_existing_tool_call_keys(
    graphql: &GraphqlEndpoint,
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

/// Whether ANSI styling should be applied: only on a real terminal, and only
/// when the caller has not opted out via `NO_COLOR`
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

struct WorkingIndicator {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

fn spinner_enabled(stdout_tty: bool, stderr_tty: bool) -> bool {
    stdout_tty && stderr_tty
}

impl WorkingIndicator {
    fn start_if_tty() -> Option<Self> {
        if !spinner_enabled(io::stdout().is_terminal(), io::stderr().is_terminal()) {
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
    eprint!("\r\x1b[2K");
    let _ = io::stderr().flush();
}

async fn clear_indicator(indicator: &mut Option<WorkingIndicator>) {
    if let Some(indicator) = indicator.take() {
        indicator.clear().await;
    }
}

fn folded_input_completed(state: RequestLifecycleState, reason: &str) -> bool {
    state == RequestLifecycleState::Superseded && reason == gents::lifecycle::FOLDED_REASON
}

#[derive(Clone, Copy)]
pub(crate) enum ApprovalHandling<'a> {
    None,
    Answer(&'a std::path::Path),
    Observe(&'a std::path::Path),
}

fn pending_approvals_home<'a>(
    mode: ApprovalHandling<'a>,
    session: &str,
) -> Result<Option<&'a std::path::Path>, StreamError> {
    let home = match mode {
        ApprovalHandling::None => return Ok(None),
        ApprovalHandling::Answer(home) | ApprovalHandling::Observe(home) => home,
    };
    Ok(super::approvals::has_pending(home, session)?.then_some(home))
}

async fn approval_progress(mode: ApprovalHandling<'_>, session: &str) -> Result<bool> {
    let Some(home) = pending_approvals_home(mode, session)? else {
        return Ok(false);
    };
    answer_pending_approvals(mode, home, session).await?;
    Ok(true)
}

async fn answer_pending_approvals(
    mode: ApprovalHandling<'_>,
    home: &std::path::Path,
    session: &str,
) -> Result<()> {
    if matches!(mode, ApprovalHandling::Answer(_)) {
        let home = home.to_path_buf();
        let session = session.to_owned();
        tokio::task::spawn_blocking(move || {
            super::approvals::answer_pending(&home, &session, |request| {
                super::approvals::ask_on_terminal(request)
            })
        })
        .await??;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the submitted request plus independent CLI polling and rendering options"
)]
pub(crate) async fn stream_turn_progress(
    graphql: &GraphqlEndpoint,
    submitted: &SubmittedRequest,
    mut known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
    verbose: bool,
    approval_handling: ApprovalHandling<'_>,
    render_folded_output: bool,
) -> Result<RequestOutputEnvelope> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut latest_progress_marker: Option<ChatProgressMarker> = None;
    let mut thinking_printed = false;
    let mut tool_output_fingerprints = std::collections::BTreeMap::new();
    let colors = colors_enabled();
    let mut indicator = matches!(approval_handling, ApprovalHandling::Answer(_))
        .then(WorkingIndicator::start_if_tty)
        .flatten();

    loop {
        if let Some(home) = pending_approvals_home(approval_handling, &submitted.session_id)? {
            clear_indicator(&mut indicator).await;
            answer_pending_approvals(approval_handling, home, &submitted.session_id).await?;
            last_progress_at = tokio::time::Instant::now();
        }
        let query = chat_progress_query(submitted);
        let response = post_graphql(graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .ok_or(StreamError::QueryOmittedRows)?;
        if rows.len() != 1 {
            return Err(StreamError::RequestRowMissing.into());
        }
        let request_row = rows.first().cloned();
        let request = request_row
            .as_ref()
            .map(|row| {
                serde_json::from_value::<AgentRequestRow>(row.clone())
                    .map_err(StreamError::DecodeRequestRow)
            })
            .transpose()?;

        if let Some(request) = request.as_ref() {
            if !(request.node_did.as_deref() == Some(submitted.node_did.as_str())
                && request.requester_did == submitted.requester_did
                && request.session_id.as_deref() == Some(submitted.session_id.as_str()))
            {
                return Err(StreamError::RequestScopeMismatch.into());
            }
        }

        let folded = request
            .as_ref()
            .is_some_and(|row| gents::lifecycle::folded_into(row).is_some());
        if folded
            || request
                .as_ref()
                .is_some_and(|row| gents::lifecycle::replaced_by(row).is_some())
        {
            clear_indicator(&mut indicator).await;
            let envelope = crate::request_helpers::wait_for_terminal_response_with_progress(
                graphql,
                &submitted.request_id,
                timeout_secs,
                poll_secs,
                || approval_progress(approval_handling, &submitted.session_id),
            )
            .await?;
            if render_folded_output {
                if let crate::CliOutputObservation::TerminalMessage { presentation, .. } =
                    &envelope.output
                {
                    if !presentation.body_markdown.trim().is_empty() {
                        println!("{}", presentation.body_markdown);
                    }
                }
                if let Some(reason) = envelope
                    .request
                    .failure_reason
                    .as_deref()
                    .filter(|reason| !reason.trim().is_empty())
                {
                    println!("[agent error] {reason}");
                }
            }
            if folded {
                println!("[input applied to the active turn]");
            }
            io::stdout().flush()?;
            return Ok(envelope);
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
                return Err(StreamError::IdleTimeout {
                    wait: format!("canonical tool output for request {}", submitted.request_id),
                    timeout_secs,
                    hint: request_diagnostic_hint(&submitted.request_id),
                }
                .into());
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
            let request = request.as_ref().ok_or(StreamError::TerminalRowLost)?;
            let output = observed_output
                .take()
                .ok_or(StreamError::TerminalOutputMissing)?;
            match output {
                gents::session::CanonicalRequestOutput::Loading => {
                    if last_progress_at.elapsed() >= idle_timeout {
                        return Err(StreamError::IdleTimeout {
                            wait: format!("materialized AgentMessage {}", submitted.request_id),
                            timeout_secs,
                            hint: request_diagnostic_hint(&submitted.request_id),
                        }
                        .into());
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
                gents::session::CanonicalRequestOutput::Denied => {
                    return Err(StreamError::TerminalOutputUnusable {
                        request_id: submitted.request_id.clone(),
                        state: "denied",
                    }
                    .into());
                }
                gents::session::CanonicalRequestOutput::Conflicted => {
                    return Err(StreamError::TerminalOutputUnusable {
                        request_id: submitted.request_id.clone(),
                        state: "conflicted",
                    }
                    .into());
                }
                gents::session::CanonicalRequestOutput::Invalid => {
                    return Err(StreamError::TerminalOutputUnusable {
                        request_id: submitted.request_id.clone(),
                        state: "invalid",
                    }
                    .into());
                }
                gents::session::CanonicalRequestOutput::Retracted => {
                    return Err(StreamError::TerminalOutputUnusable {
                        request_id: submitted.request_id.clone(),
                        state: "retracted",
                    }
                    .into());
                }
                gents::session::CanonicalRequestOutput::RetainedPartial(_) => {
                    return Err(StreamError::RetainedPartialOnly {
                        request_id: submitted.request_id.clone(),
                    }
                    .into());
                }
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
                    if folded_input_completed(envelope.request.lifecycle_state, error_message) {
                        println!("[input applied to the active turn]");
                    } else if !error_message.is_empty() {
                        println!("[agent error] {error_message}");
                        let agent_id = request.agent_id.as_deref();
                        for line in
                            account_problems(graphql, submitted, agent_id, error_message).await
                        {
                            println!("[account] {line}");
                        }
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
                    if last_progress_at.elapsed() >= idle_timeout {
                        return Err(StreamError::IdleTimeout {
                            wait: format!(
                                "terminal canonical selection for request {}",
                                submitted.request_id
                            ),
                            timeout_secs,
                            hint: request_diagnostic_hint(&submitted.request_id),
                        }
                        .into());
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            return Err(StreamError::IdleTimeout {
                wait: format!("canonical output for request {}", submitted.request_id),
                timeout_secs,
                hint: request_diagnostic_hint(&submitted.request_id),
            }
            .into());
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

fn render_tool_line(tool: &ToolCallProgress, verbose: bool, colors: bool) -> String {
    if verbose {
        format_tool_progress_line(tool)
    } else {
        format_tool_summary_line(tool, colors)
    }
}

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

pub(super) const SUMMARY_ARGUMENT_MAX_CHARS: usize = 100;

/// Removes terminal control characters from untrusted text before display.
pub(crate) fn sanitize_summary_text(text: &str, max_chars: usize) -> Option<String> {
    let mut normalized = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        } else if ch.is_control() {
            // ESC, BEL, backspace, and friends: printed verbatim these are
            // live terminal controls, not text, so they are dropped rather
            // than escaped — there is no safe visible rendering for them in
            // a plain one-line summary.
            continue;
        } else {
            normalized.push(ch);
            last_was_space = false;
        }
    }
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(bounded_chars(trimmed, max_chars))
}

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

fn tool_outcome(status: &str, result: Option<&str>) -> Option<ToolOutcome> {
    match status {
        "completed" => Some(match result.and_then(parse_tool_output_metadata) {
            Some((ok, meta_status)) if !ok => ToolOutcome::Failed(sanitize_summary_text(
                &meta_status,
                SUMMARY_ARGUMENT_MAX_CHARS,
            )),
            _ => ToolOutcome::Ok,
        }),
        "error" => Some(ToolOutcome::Failed(result.and_then(|value| {
            sanitize_summary_text(value, SUMMARY_ARGUMENT_MAX_CHARS)
        }))),
        _ => None,
    }
}

pub(super) fn format_tool_summary_line(tool: &ToolCallProgress, colors: bool) -> String {
    let mut line = sanitize_summary_text(&tool.tool_name, SUMMARY_ARGUMENT_MAX_CHARS)
        .unwrap_or_else(|| "tool".to_string());
    let argument = key_argument(&tool.arguments)
        .or_else(|| preview_compact_text(&tool.arguments))
        .and_then(|argument| sanitize_summary_text(&argument, SUMMARY_ARGUMENT_MAX_CHARS));
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
    let mut hasher = rapidhash::fast::RapidHasher::default();
    value.hash(&mut hasher);
    (value.len(), hasher.finish())
}

fn account_problem_lines(
    failure_reason: &str,
    accounts: &[(String, gents::oauth_credential::ServingAccount)],
    all: &[gents::oauth_credential::AccountSummary],
) -> Vec<String> {
    if !gents_protocol::node_readiness::is_behavior_unavailable_rejection(failure_reason.trim()) {
        return Vec::new();
    }
    accounts
        .iter()
        .filter(|(_, account)| account.state != gents::oauth_credential::AccountState::Enabled)
        .filter_map(|(profile, account)| {
            let provider = account.provider?;
            Some(format!(
                "profile {profile} uses account {:?} ({}); move it: gents config profile \
                 set-account {profile} <account> ({})",
                account.label,
                account.state.as_str(),
                gents::oauth_credential::enabled_accounts_note(provider, all),
            ))
        })
        .collect()
}

async fn account_problems(
    graphql: &GraphqlEndpoint,
    submitted: &SubmittedRequest,
    agent_id: Option<&str>,
    failure_reason: &str,
) -> Vec<String> {
    let (Some(agent_id), true) = (
        agent_id,
        gents_protocol::node_readiness::is_behavior_unavailable_rejection(failure_reason.trim()),
    ) else {
        return Vec::new();
    };
    let read = async {
        let access = gents::config_client::ConfigAccess::Graphql(graphql.clone());
        let did = submitted.node_did.as_str();
        anyhow::Ok((
            gents::config_client::agent_accounts(&access, did, agent_id).await?,
            gents::oauth_credential::list_accounts(&access, did).await?,
        ))
    };
    match read.await {
        Ok((accounts, all)) => account_problem_lines(failure_reason, &accounts, &all),
        Err(error) => {
            tracing::debug!(%error, "accounts of a refused turn unavailable");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_progress_query_omits_retired_tool_payload_fields() {
        let submitted = SubmittedRequest {
            request_id: "request-one".to_string(),
            session_id: "session-one".to_string(),
            node_did: "did:key:agent".to_string(),
            agent_id: None,
            request_doc_id: "doc-one".to_string(),
            requester_did: Some("did:key:requester".to_string()),
            input: None,
            created_at: None,
        };

        let query = chat_progress_query(&submitted);

        assert!(!query.contains(" args"));
        assert!(!query.contains(" result"));
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
    fn tool_summary_line_sanitizes_a_hostile_tool_name_and_status_and_keeps_dim_ansi() {
        let hostile_name = format!("bash\x1b[31mHACKED\x1b[0m\r\n{}", "n".repeat(200));
        let hostile_status = format!("exit\x1b[31mHACKED\x1b[0m\r\n{}", "s".repeat(200));
        let result = format!(
            "gents_exec: {}\nstdout:\n(empty)\nstderr:\n(empty)",
            serde_json::json!({"ok": false, "status": hostile_status, "exit_code": 1})
        );
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: hostile_name,
            status: "completed".into(),
            arguments: r#"{"command":"ls"}"#.into(),
            result: Some(result),
        };

        let plain = format_tool_summary_line(&tool, false);
        assert!(
            !plain.contains('\u{1b}'),
            "ESC leaked into plain line: {plain:?}"
        );
        assert!(
            !plain.contains('\r'),
            "CR leaked into plain line: {plain:?}"
        );
        assert_eq!(
            plain.lines().count(),
            1,
            "summary must stay one line: {plain:?}"
        );
        assert!(
            plain.chars().count() < 250,
            "summary line should stay bounded even for hostile name/status: {plain}"
        );

        let colored = format_tool_summary_line(&tool, true);
        assert!(
            colored.starts_with(DIM) && colored.ends_with(RESET),
            "deliberate dim ANSI wrapper must remain: {colored:?}"
        );
        let inner = colored
            .strip_prefix(DIM)
            .and_then(|rest| rest.strip_suffix(RESET))
            .expect("dim wrapper present");
        assert!(
            !inner.contains('\u{1b}'),
            "ESC leaked inside the dim wrapper: {inner:?}"
        );
        assert!(
            !inner.contains('\r'),
            "CR leaked inside the dim wrapper: {inner:?}"
        );
    }

    #[test]
    fn verbose_tool_line_is_unaffected_by_the_short_summary_formatter() {
        let tool = bash_tool(
            "completed",
            Some("gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\"}"),
        );
        assert_eq!(
            render_tool_line(&tool, true, false),
            format_tool_progress_line(&tool)
        );
        assert!(render_tool_line(&tool, true, false).contains("exit_nonzero"));
    }

    #[test]
    fn sanitize_summary_text_collapses_a_multiline_command_to_one_line() {
        assert_eq!(
            sanitize_summary_text("echo line1\nline2\r\nline3", 100),
            Some("echo line1 line2 line3".to_string())
        );
    }

    #[test]
    fn sanitize_summary_text_strips_esc_and_neutralizes_cr() {
        let input = "ls \u{1b}[31mHACKED\u{1b}[0m\rdone";
        let sanitized = sanitize_summary_text(input, 100).expect("nonempty");
        assert!(!sanitized.contains('\u{1b}'), "ESC leaked: {sanitized:?}");
        assert!(!sanitized.contains('\r'), "CR leaked: {sanitized:?}");
        assert_eq!(sanitized, "ls [31mHACKED[0m done");
    }

    #[test]
    fn sanitize_summary_text_drops_other_control_characters() {
        assert_eq!(
            sanitize_summary_text("a\u{7}b\u{8}c", 100),
            Some("abc".to_string())
        );
    }

    #[test]
    fn sanitize_summary_text_bounds_a_very_long_input() {
        let long = "x".repeat(500);
        let sanitized = sanitize_summary_text(&long, 100).expect("nonempty");
        assert_eq!(sanitized.chars().count(), 103); // 100 + "..."
        assert!(sanitized.ends_with("..."));
        assert!(sanitized.starts_with(&"x".repeat(100)));
    }

    #[test]
    fn sanitize_summary_text_cuts_on_a_char_boundary_near_non_ascii() {
        let mut input = "a".repeat(99);
        input.push_str("考考考考考");
        let sanitized = sanitize_summary_text(&input, 100).expect("nonempty");
        assert_eq!(sanitized.chars().count(), 103); // 100 chars + "..."
        assert!(sanitized.ends_with("..."));
        assert!(sanitized.is_char_boundary(sanitized.len()));
    }

    #[test]
    fn sanitize_summary_text_is_none_for_only_whitespace_and_controls() {
        assert_eq!(sanitize_summary_text("  \n\t \u{1b} ", 100), None);
        assert_eq!(sanitize_summary_text("", 100), None);
    }

    #[test]
    fn tool_summary_line_collapses_a_multiline_bash_command_to_one_line() {
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "bash".into(),
            status: "running".into(),
            arguments: r#"{"command":"printf 'line1\nline2\u001b[31mline3'"}"#.into(),
            result: None,
        };
        let line = format_tool_summary_line(&tool, false);
        assert_eq!(
            line.lines().count(),
            1,
            "summary must stay one line: {line:?}"
        );
        assert!(
            !line.contains('\u{1b}'),
            "ESC leaked into summary: {line:?}"
        );
        assert_eq!(line, "  [tool] bash printf 'line1 line2[31mline3'");
    }

    #[test]
    fn tool_summary_line_bounds_a_very_long_argument() {
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "bash".into(),
            status: "running".into(),
            arguments: format!(r#"{{"command":"{}"}}"#, "x".repeat(500)),
            result: None,
        };
        let line = format_tool_summary_line(&tool, false);
        assert!(line.contains("..."), "expected an elided argument: {line}");
        assert!(
            line.chars().count() < 150,
            "summary line should stay short even for a huge argument: {line}"
        );
    }

    #[test]
    fn spinner_enabled_requires_both_stdout_and_stderr_to_be_terminals() {
        assert!(spinner_enabled(true, true));
        assert!(!spinner_enabled(true, false));
        assert!(!spinner_enabled(false, true));
        assert!(!spinner_enabled(false, false));
    }

    mod account_problem {
        use super::*;
        use gents::oauth_credential::{AccountState, AccountSummary, ServingAccount};
        use gents_protocol::row::AgentReadinessUnavailableReason as Reason;

        const CLAUDE: &str = "claude-subscription";

        fn on(profile: &str, label: &str, state: AccountState) -> (String, ServingAccount) {
            (
                profile.into(),
                ServingAccount {
                    label: label.into(),
                    state,
                    provider: Some(CLAUDE),
                },
            )
        }

        fn summary(label: &str, enabled: bool) -> AccountSummary {
            AccountSummary {
                credential_id: format!("{CLAUDE}:did:key:z6MkTest:{label}"),
                provider: CLAUDE.into(),
                account_ref: None,
                label: label.into(),
                identity: Some("IDENTITY".into()),
                plan: None,
                enabled,
                default: false,
                access_token_expires_at: chrono::Utc::now(),
                connected_at: None,
            }
        }

        fn all() -> Vec<AccountSummary> {
            vec![summary("Claude", true), summary("label-b", false)]
        }

        #[test]
        fn account_problem_lines_name_the_disabled_account() {
            let lines = account_problem_lines(
                Reason::CredentialsRequired.public_message(),
                &[on("p", "label-b", AccountState::Disabled)],
                &all(),
            );
            assert_eq!(
                lines,
                [
                    "profile p uses account \"label-b\" (disabled); move it: gents config profile \
                  set-account p <account> (enabled Claude accounts: \"Claude\")"
                ]
            );
        }

        #[test]
        fn account_problem_lines_name_a_missing_account_or_the_login() {
            let missing = account_problem_lines(
                Reason::CredentialsRequired.public_message(),
                &[on("p", "label-gone", AccountState::Missing)],
                &all(),
            );
            assert_eq!(missing.len(), 1);
            assert!(
                missing[0].contains("(account not on this node)"),
                "{missing:?}"
            );
            let none = account_problem_lines(
                Reason::CredentialsRequired.public_message(),
                &[on("p", "label-b", AccountState::Disabled)],
                &[summary("label-b", false)],
            );
            assert!(none[0].contains("gents claude-login"), "{none:?}");
        }

        #[test]
        fn account_problem_lines_name_the_compaction_profile() {
            let lines = account_problem_lines(
                Reason::ToolConfigurationInvalid.public_message(),
                &[
                    on("p", "Claude", AccountState::Enabled),
                    on("p-compaction", "label-b", AccountState::Disabled),
                ],
                &all(),
            );
            assert_eq!(lines.len(), 1);
            assert!(
                lines[0].starts_with("profile p-compaction uses account \"label-b\""),
                "{lines:?}"
            );
        }

        #[test]
        fn account_problem_lines_are_silent_otherwise() {
            let disabled = [on("p", "label-b", AccountState::Disabled)];
            assert!(account_problem_lines("provider returned 500", &disabled, &all()).is_empty());
            assert!(account_problem_lines(
                Reason::CredentialsRequired.public_message(),
                &[on("p", "Claude", AccountState::Enabled)],
                &all(),
            )
            .is_empty());
            let lines = account_problem_lines(
                Reason::CredentialsRequired.public_message(),
                &disabled,
                &all(),
            );
            assert!(!format!("{lines:?}").contains("IDENTITY"));
        }
    }
}

#[cfg(test)]
mod folded_input_tests {
    use super::*;

    #[test]
    fn only_owned_fold_supersession_is_an_informational_completion() {
        let reason = gents::lifecycle::FOLDED_REASON;
        assert!(folded_input_completed(
            RequestLifecycleState::Superseded,
            reason
        ));
        assert!(!folded_input_completed(
            RequestLifecycleState::Failed,
            reason
        ));
        assert!(!folded_input_completed(
            RequestLifecycleState::Superseded,
            "another failure"
        ));
    }
}

#[cfg(test)]
mod approval_progress_tests {
    use super::*;

    #[tokio::test]
    async fn observing_pending_approval_reports_progress_without_answering() {
        use gents::pack::BindAccess;
        use gents::plugin::{allowed::Resolved, approval};
        let home = tempfile::tempdir().unwrap();
        let request = approval::Request::new(
            "fixture/plugin",
            &Resolved {
                target: home.path().join("requested-file"),
                is_dir: false,
            },
            BindAccess::Read,
            Some("held-session".into()),
        );
        let dir = approval::queue_dir(home.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", request.id)),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        assert!(!approval_progress(ApprovalHandling::None, "held-session")
            .await
            .unwrap());
        assert!(
            !approval_progress(ApprovalHandling::Observe(home.path()), "other-session")
                .await
                .unwrap()
        );
        assert!(
            approval_progress(ApprovalHandling::Observe(home.path()), "held-session")
                .await
                .unwrap()
        );
        assert_eq!(
            approval::pending(home.path()).unwrap(),
            vec![request.clone()]
        );
        approval::decide(home.path(), &request.id, approval::Answer::Deny).unwrap();
        assert!(
            !approval_progress(ApprovalHandling::Observe(home.path()), "held-session")
                .await
                .unwrap()
        );
    }
}
