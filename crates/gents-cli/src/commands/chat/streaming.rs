use std::hash::{Hash, Hasher};
use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::operations::request::{RequestReceipt, RequestRef};
use gents_protocol::row::AgentToolCallRow;
use gents_protocol::session::SessionScope;
use gents_session::views::transcript::ToolCallRows;
use gents_store::config_client::ConfigAccess;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::operation_failure::OperationFailure;
use crate::request_helpers::wait_for_request;
use crate::RequestOutputEnvelope;

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

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

/// What the turn's progress lines have shown so far. The request owner's wait
/// does not read tool calls or the live answer, so the stream reads them itself
/// on each of its polls.
struct Shown {
    known_tool_calls: std::collections::BTreeMap<String, String>,
    tool_output_fingerprints: std::collections::BTreeMap<String, Option<(usize, u64)>>,
    tool_markers: Vec<ChatToolProgressMarker>,
    thinking_printed: bool,
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

fn folded_input_completed(state: RequestLifecycleState, reason: &str) -> bool {
    state == RequestLifecycleState::Superseded
        && reason == gents_session::request::lifecycle::FOLDED_REASON
}

/// Prints the tool lines and the thinking indicator that changed since the
/// last poll; true when the turn showed progress.
async fn show_progress(
    access: &ConfigAccess,
    receipt: &RequestReceipt,
    request: &RequestRef,
    shown: &Mutex<Shown>,
    verbose: bool,
    colors: bool,
) -> Result<bool> {
    let mut shown = shown.lock().await;
    let tool_rows = access
        .read_view(&ToolCallRows::for_request(
            SessionScope {
                node_did: receipt.node_did.clone(),
                session_id: receipt.session_id.clone(),
                requester_did: receipt.requester_did.clone(),
            },
            &receipt.request_doc_id,
        ))
        .await?;
    let mut progressed = false;
    for mut tool in tool_rows.iter().filter_map(tool_call_progress) {
        let terminal = matches!(tool.status.as_str(), "completed" | "error");
        let presentation = gents_session::session::load_tool_call_presentation(
            access,
            &tool.tool_call_doc_id,
            &receipt.node_did,
            &receipt.session_id,
            receipt.requester_did.as_deref(),
        )
        .await
        .context("loading canonical tool presentation")?;
        tool.arguments = presentation.arguments;
        let published = terminal.then_some(presentation.result).flatten();
        if terminal && published.is_none() {
            tool.result = presentation.live_output;
            let output_fingerprint = tool.result.as_deref().map(text_fingerprint);
            let previous_fingerprint = shown
                .tool_output_fingerprints
                .get(&tool.tool_call_key)
                .copied()
                .flatten();
            if previous_fingerprint != output_fingerprint {
                shown
                    .tool_output_fingerprints
                    .insert(tool.tool_call_key.clone(), output_fingerprint);
                progressed = true;
                println!("{}", render_live_tool_line(&tool, verbose, colors));
                io::stdout().flush()?;
            }
            continue;
        }
        tool.result = published.or(presentation.live_output);
        let output_fingerprint = tool.result.as_deref().map(text_fingerprint);
        let previous_status = shown.known_tool_calls.get(&tool.tool_call_key).cloned();
        let previous_fingerprint = shown
            .tool_output_fingerprints
            .get(&tool.tool_call_key)
            .copied()
            .flatten();
        if previous_status.as_deref() == Some(tool.status.as_str())
            && previous_fingerprint == output_fingerprint
        {
            continue;
        }
        shown
            .tool_output_fingerprints
            .insert(tool.tool_call_key.clone(), output_fingerprint);
        shown
            .known_tool_calls
            .insert(tool.tool_call_key.clone(), tool.status.clone());
        progressed = true;
        if previous_status.is_none() && terminal {
            println!("{}", render_tool_start_line(&tool, verbose, colors));
        }
        println!("{}", render_tool_line(&tool, verbose, colors));
        io::stdout().flush()?;
    }
    let markers: Vec<_> = tool_rows.iter().map(chat_tool_progress_marker).collect();
    if markers != shown.tool_markers {
        shown.tool_markers = markers;
        progressed = true;
    }
    if !shown.thinking_printed {
        let row = gents_session::operations::request::get(access, request)
            .await
            .map_err(OperationFailure::from)?;
        let output = gents_session::session::observe_request_output(access, &row).await?;
        if should_print_thinking(row.lifecycle_state, &output) {
            println!("{}", dim("[thinking]", colors));
            io::stdout().flush()?;
            shown.thinking_printed = true;
        }
    }
    Ok(progressed)
}

pub(crate) async fn stream_turn_progress(
    access: &ConfigAccess,
    receipt: &RequestReceipt,
    known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
    verbose: bool,
    render_folded_output: bool,
) -> Result<RequestOutputEnvelope> {
    let colors = colors_enabled();
    let request = RequestRef {
        node_did: receipt.node_did.clone(),
        requester_did: receipt.requester_did.clone(),
        request_id: receipt.request_id.clone(),
    };
    let shown = Mutex::new(Shown {
        known_tool_calls,
        tool_output_fingerprints: std::collections::BTreeMap::new(),
        tool_markers: Vec::new(),
        thinking_printed: false,
    });
    let envelope = wait_for_request(access, request.clone(), timeout_secs, poll_secs, || {
        show_progress(access, receipt, &request, &shown, verbose, colors)
    })
    .await?;
    show_progress(access, receipt, &request, &shown, verbose, colors).await?;

    if envelope.request.request_id != receipt.request_id {
        let original = gents_session::operations::request::get(access, &request)
            .await
            .map_err(OperationFailure::from)?;
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
        if gents_session::request::lifecycle::folded_into(&original).is_some() {
            println!("[input applied to the active turn]");
        }
        io::stdout().flush()?;
        return Ok(envelope);
    }

    if let crate::CliOutputObservation::TerminalMessage { presentation, .. } = &envelope.output {
        if !presentation.body_markdown.trim().is_empty() {
            println!("{}", presentation.body_markdown);
            io::stdout().flush()?;
        }
    }
    let error_message = envelope
        .request
        .failure_reason
        .as_deref()
        .unwrap_or("")
        .trim();
    if folded_input_completed(envelope.request.lifecycle_state, error_message) {
        println!("[input applied to the active turn]");
    } else if !error_message.is_empty() {
        println!("[agent error] {error_message}");
        let agent_did = envelope.request.agent_did.as_deref();
        for line in account_problems(access, &receipt.node_did, agent_did, error_message).await {
            println!("[account] {line}");
        }
        println!(
            "[inspect] gents utils debug response show {}",
            receipt.request_id
        );
        io::stdout().flush()?;
    } else if matches!(
        envelope.request.lifecycle_state,
        RequestLifecycleState::Failed | RequestLifecycleState::Dead
    ) {
        println!(
            "[inspect] gents utils debug response show {}",
            receipt.request_id
        );
        io::stdout().flush()?;
    }
    Ok(envelope)
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
    output: &gents_session::session::CanonicalRequestOutput,
) -> Option<ChatOutputProgress> {
    let (kind, presentation) = match output {
        gents_session::session::CanonicalRequestOutput::Live(value) => ("live", value),
        gents_session::session::CanonicalRequestOutput::Settling(value) => ("settling", value),
        gents_session::session::CanonicalRequestOutput::Published { presentation, .. } => {
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

fn chat_tool_progress_marker(row: &AgentToolCallRow) -> ChatToolProgressMarker {
    ChatToolProgressMarker {
        tool_call_key: Some(row.tool_call_key.clone()),
        status: row.status.clone(),
        started_at: row.started_at.clone(),
        completed_at: row.completed_at.clone(),
    }
}

fn should_print_thinking(
    lifecycle_state: Option<RequestLifecycleState>,
    output: &gents_session::session::CanonicalRequestOutput,
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

fn tool_call_progress(row: &AgentToolCallRow) -> Option<ToolCallProgress> {
    Some(ToolCallProgress {
        tool_call_doc_id: row.doc_id.clone()?,
        tool_call_key: row.tool_call_key.clone(),
        tool_name: row.tool_name.clone()?,
        status: row.status.clone()?,
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
    accounts: &[(String, gents_inference::auth::credential::ServingAccount)],
    all: &[gents_inference::auth::credential::AccountSummary],
) -> Vec<String> {
    if !gents_protocol::node_readiness::is_behavior_unavailable_rejection(failure_reason.trim()) {
        return Vec::new();
    }
    accounts
        .iter()
        .filter(|(_, account)| {
            account.state != gents_inference::auth::credential::AccountState::Enabled
        })
        .filter_map(|(profile, account)| {
            let provider = account.provider?;
            Some(format!(
                "profile {profile} uses account {:?} ({}); move it: gents config profile \
                 set-account {profile} <account> ({})",
                account.label,
                account.state.as_str(),
                gents_inference::auth::credential::enabled_accounts_note(provider, all),
            ))
        })
        .collect()
}

async fn account_problems(
    access: &ConfigAccess,
    did: &str,
    agent_did: Option<&str>,
    failure_reason: &str,
) -> Vec<String> {
    let (Some(agent_did), true) = (
        agent_did,
        gents_protocol::node_readiness::is_behavior_unavailable_rejection(failure_reason.trim()),
    ) else {
        return Vec::new();
    };
    let read = async {
        anyhow::Ok((
            gents_inference::auth::credential::agent_accounts(access, did, agent_did).await?,
            gents_inference::auth::credential::list_accounts(access, did).await?,
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
    fn compact_preview_is_unicode_safe_and_bounded() {
        let preview = preview_compact_text(&"考".repeat(121)).expect("nonempty preview");
        assert_eq!(preview.chars().count(), 123);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn thinking_indicator_follows_active_lifecycle_and_projected_reasoning() {
        let reasoning_only = gents_session::session::CanonicalRequestOutput::Live(
            gents_session::session::CanonicalPresentation {
                body_markdown: String::new(),
                reasoning_markdown: Some("pondering the request".to_string()),
                selected_source: None,
            },
        );
        assert!(should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &reasoning_only
        ));

        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Claimed),
            &reasoning_only
        ));

        let with_content = gents_session::session::CanonicalRequestOutput::Live(
            gents_session::session::CanonicalPresentation {
                body_markdown: "answer body".to_string(),
                reasoning_markdown: Some("pondering the request".to_string()),
                selected_source: None,
            },
        );
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &with_content
        ));

        let no_reasoning = gents_session::session::CanonicalRequestOutput::Live(
            gents_session::session::CanonicalPresentation {
                body_markdown: String::new(),
                reasoning_markdown: None,
                selected_source: None,
            },
        );
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &no_reasoning
        ));
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &gents_session::session::CanonicalRequestOutput::Loading
        ));
        assert!(!should_print_thinking(None, &reasoning_only));
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

    mod account_problem {
        use super::*;
        use gents_inference::auth::credential::{AccountState, AccountSummary, ServingAccount};
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
            assert!(none[0].contains("gents accounts login claude"), "{none:?}");
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
        let reason = gents_session::request::lifecycle::FOLDED_REASON;
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
