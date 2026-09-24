mod streaming;

use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::args::ChatArgs;
use crate::cli::output_format::OutputFormat;
use crate::request_helpers::{create_goal_backed_agent_request, ensure_local_request_signer};
use crate::{
    create_agent_request, print_json, require_non_empty, resolve_home_dir,
    wait_for_terminal_response, write_json_output_file, RequestOutputEnvelope,
    RequestSubmitOptions, SubmittedRequest, DEFAULT_HTTP_PORT,
};

use streaming::{
    load_existing_tool_call_keys, sanitize_summary_text, stream_turn_progress,
    SUMMARY_ARGUMENT_MAX_CHARS,
};

pub(crate) async fn chat(args: ChatArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let runtime_state = crate::read_runtime_state(&home_dir)?;
    let init_config = crate::read_init_config(&home_dir)?;
    let graphql = args
        .graphql
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.graphql.clone()))
        .unwrap_or_else(|| format!("http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql"));
    let agent_did = match args
        .agent_did
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_did.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_did.clone()))
    {
        Some(agent_did) => agent_did,
        None => bail!(
            "agent DID is required; run `gents init`, start `gents server`, then retry `gents chat`, or pass --agent-did explicitly"
        ),
    };
    let session_id = args
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    ensure_local_request_signer(args.home.as_deref(), &agent_did)?;
    let goal = args
        .goal_objective
        .as_deref()
        .map(|objective| GoalBackedSubmission {
            objective,
            token_budget: args.goal_token_budget,
        });

    if let Some(message) = resolve_chat_message(&args.message, args.message_file.as_deref())? {
        match args
            .output_format
            .ensure_supported("chat", &[OutputFormat::Text, OutputFormat::Json])?
        {
            OutputFormat::Text => {
                let envelope = submit_chat_turn_with_goal(
                    &graphql,
                    &agent_did,
                    &session_id,
                    args.behavior_id.as_deref(),
                    &message,
                    goal,
                    args.timeout_secs,
                    args.poll_secs,
                    args.verbose,
                )
                .await?;
                if let Some(path) = args.output_file.as_deref() {
                    write_text_output_file(path, chat_turn_text_content(&envelope))?;
                }
            }
            OutputFormat::Json => {
                let output = submit_chat_turn_json(
                    &graphql,
                    &agent_did,
                    &session_id,
                    args.behavior_id.as_deref(),
                    &message,
                    goal,
                    args.timeout_secs,
                    args.poll_secs,
                )
                .await?;
                print_json(&output)?;
                if let Some(path) = args.output_file.as_deref() {
                    write_json_output_file(path, &output)?;
                }
            }
            _ => unreachable!("ensure_supported restricts chat output formats"),
        }
        return Ok(());
    }

    if args.output_format != OutputFormat::Text {
        anyhow::bail!("interactive chat only supports --output text");
    }
    if let Some(path) = args.output_file.as_deref() {
        anyhow::bail!(
            "--output-file {} requires a one-shot message via MESSAGE or --message-file",
            path.display()
        );
    }

    let prompt_label = chat_prompt_label(&args, runtime_state.as_ref());

    let stdin = io::stdin();
    let mut pending_goal = goal;
    let mut lines = stdin.lock().lines();
    let mut stdout = io::stdout();
    loop {
        write!(stdout, "{prompt_label}> ")?;
        stdout.flush()?;
        let Some(line) = lines.next() else {
            break;
        };
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "/exit" | "/quit" | "exit" | "quit") {
            break;
        }

        submit_chat_turn_with_goal(
            &graphql,
            &agent_did,
            &session_id,
            args.behavior_id.as_deref(),
            trimmed,
            pending_goal.take(),
            args.timeout_secs,
            args.poll_secs,
            args.verbose,
        )
        .await?;
    }

    Ok(())
}

/// Minimal, one-line context for the interactive prompt: which agent is
/// listening. Falls back through the sources that can name it, and finally
/// to a generic label rather than a bare `>` with no context (#1622).
fn chat_prompt_label(
    args: &ChatArgs,
    runtime_state: Option<&crate::shared::StoredRuntimeState>,
) -> String {
    sanitize_prompt_label(
        args.agent_name
            .clone()
            .or_else(|| runtime_state.map(|state| state.agent_name.clone())),
    )
}

/// Routes a candidate agent-name label through the same control/newline
/// stripping and bounding used for tool summaries: `agent_name` can come
/// from `--agent-name` or stored runtime state, neither of which is trusted
/// terminal input, and this prompt is printed on every turn.
fn sanitize_prompt_label(name: Option<String>) -> String {
    name.and_then(|name| sanitize_summary_text(&name, SUMMARY_ARGUMENT_MAX_CHARS))
        .unwrap_or_else(|| "gents".to_string())
}

#[derive(Clone, Copy)]
struct GoalBackedSubmission<'a> {
    objective: &'a str,
    token_budget: Option<i64>,
}

async fn submit_chat_turn_with_goal(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    content: &str,
    goal: Option<GoalBackedSubmission<'_>>,
    timeout_secs: u64,
    poll_secs: u64,
    verbose: bool,
) -> Result<RequestOutputEnvelope> {
    let existing_tool_calls = load_existing_tool_call_keys(graphql, session_id).await?;
    let submitted = match goal {
        Some(goal) => {
            create_goal_backed_agent_request(
                graphql,
                agent_did,
                content,
                session_id,
                behavior_id,
                goal.objective,
                goal.token_budget,
            )
            .await?
        }
        None => {
            create_agent_request(
                graphql,
                agent_did,
                content,
                Some(session_id),
                behavior_id,
                RequestSubmitOptions::default(),
            )
            .await?
        }
    };
    stream_turn_progress(
        graphql,
        &submitted,
        existing_tool_calls,
        timeout_secs,
        poll_secs,
        verbose,
    )
    .await
}

async fn submit_chat_turn_json(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    content: &str,
    goal: Option<GoalBackedSubmission<'_>>,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<Value> {
    let submitted = match goal {
        Some(goal) => {
            create_goal_backed_agent_request(
                graphql,
                agent_did,
                content,
                session_id,
                behavior_id,
                goal.objective,
                goal.token_budget,
            )
            .await?
        }
        None => {
            create_agent_request(
                graphql,
                agent_did,
                content,
                Some(session_id),
                behavior_id,
                RequestSubmitOptions::default(),
            )
            .await?
        }
    };
    let envelope =
        wait_for_terminal_response(graphql, &submitted.request_id, timeout_secs, poll_secs)
            .await
            .with_context(|| {
                format!(
                    "waiting for request {} to reach a terminal lifecycle state",
                    submitted.request_id
                )
            })?;
    Ok(chat_turn_output(&submitted, envelope))
}

fn resolve_chat_message(message: &[String], message_file: Option<&Path>) -> Result<Option<String>> {
    if !message.is_empty() && message_file.is_some() {
        anyhow::bail!("provide either MESSAGE or --message-file, not both");
    }
    if !message.is_empty() {
        return Ok(Some(
            require_non_empty("message", &message.join(" "))?.to_string(),
        ));
    }
    if let Some(path) = message_file {
        let message = std::fs::read_to_string(path)
            .with_context(|| format!("reading chat message from {}", path.display()))?;
        return Ok(Some(
            require_non_empty("message-file", &message)?.to_string(),
        ));
    }
    Ok(None)
}

fn write_text_output_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    std::fs::write(path, content)
        .with_context(|| format!("writing text output file {}", path.display()))?;
    Ok(())
}

/// Terminal envelope selection only; `TerminalMessage` carries the exact
/// terminal presentation, and `TerminalNoMessage` is explicit absence.
fn terminal_presentation(
    envelope: &RequestOutputEnvelope,
) -> Option<&crate::CliOutputPresentation> {
    match &envelope.output {
        crate::CliOutputObservation::TerminalMessage { presentation, .. } => Some(presentation),
        crate::CliOutputObservation::TerminalNoMessage => None,
        _ => None,
    }
}

fn chat_turn_text_content(envelope: &RequestOutputEnvelope) -> &str {
    terminal_presentation(envelope)
        .map(|presentation| presentation.body_markdown.as_str())
        .unwrap_or("")
}

fn chat_turn_output(submitted: &SubmittedRequest, envelope: RequestOutputEnvelope) -> Value {
    let request = serde_json::to_value(&envelope.request).unwrap_or(serde_json::Value::Null);
    let output = serde_json::to_value(&envelope.output).unwrap_or(serde_json::Value::Null);
    json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
        "request": request,
        "output": output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_label_strips_control_characters_and_bounds_a_hostile_agent_name() {
        let hostile = format!("gents\x1b[31mHACKED\x1b[0m\r\n{}", "x".repeat(200));
        let label = sanitize_prompt_label(Some(hostile));
        assert!(!label.contains('\u{1b}'), "ESC leaked: {label:?}");
        assert!(!label.contains('\r'), "CR leaked: {label:?}");
        assert!(
            !label.contains('\n'),
            "prompt label must stay one line: {label:?}"
        );
        assert!(label.chars().count() <= SUMMARY_ARGUMENT_MAX_CHARS + 3);
    }

    #[test]
    fn prompt_label_falls_back_to_gents_when_nothing_printable_remains() {
        assert_eq!(sanitize_prompt_label(None), "gents");
        assert_eq!(sanitize_prompt_label(Some("\x1b\x07".to_string())), "gents");
    }
}
