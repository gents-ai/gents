use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::args::{ChatArgs, ChatOutputFormat};
use crate::{
    create_agent_request, post_graphql, print_json, request_diagnostic_hint, require_non_empty,
    resolve_home_dir, wait_for_terminal_response, write_json_output_file, RequestSubmitOptions,
    SubmittedRequest, DEFAULT_AGENT_NAME, DEFAULT_HTTP_PORT,
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
    let agent_name = args
        .agent_name
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_name.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_name.clone()))
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
    let agent_did = args
        .agent_did
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_did.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_did.clone()))
        .unwrap_or_else(|| format!("did:defra-agent:{agent_name}"));
    let session_id = args
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Some(message) = resolve_chat_message(&args.message, args.message_file.as_deref())? {
        match args.output_format {
            ChatOutputFormat::Text => {
                let (_submitted, response) = submit_chat_turn(
                    &graphql,
                    &agent_did,
                    &session_id,
                    args.behavior_id.as_deref(),
                    &message,
                    args.timeout_secs,
                    args.poll_secs,
                )
                .await?;
                if let Some(path) = args.output_file.as_deref() {
                    write_text_output_file(path, response_text_content(&response))?;
                }
            }
            ChatOutputFormat::Json => {
                let output = submit_chat_turn_json(
                    &graphql,
                    &agent_did,
                    &session_id,
                    args.behavior_id.as_deref(),
                    &message,
                    args.timeout_secs,
                    args.poll_secs,
                )
                .await?;
                print_json(&output)?;
                if let Some(path) = args.output_file.as_deref() {
                    write_json_output_file(path, &output)?;
                }
            }
        }
        return Ok(());
    }

    if args.output_format != ChatOutputFormat::Text {
        anyhow::bail!("interactive chat only supports --output-format text");
    }
    if let Some(path) = args.output_file.as_deref() {
        anyhow::bail!(
            "--output-file {} requires a one-shot message via MESSAGE or --message-file",
            path.display()
        );
    }

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = io::stdout();
    loop {
        write!(stdout, "> ")?;
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

        submit_chat_turn(
            &graphql,
            &agent_did,
            &session_id,
            args.behavior_id.as_deref(),
            trimmed,
            args.timeout_secs,
            args.poll_secs,
        )
        .await?;
    }

    Ok(())
}

async fn submit_chat_turn(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    content: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<(SubmittedRequest, Value)> {
    let existing_tool_calls = load_existing_tool_call_keys(graphql, session_id).await?;
    let submitted = create_agent_request(
        graphql,
        agent_did,
        content,
        Some(session_id),
        behavior_id,
        RequestSubmitOptions::default(),
    )
    .await?;
    let response = stream_turn_progress(
        graphql,
        &submitted,
        existing_tool_calls,
        timeout_secs,
        poll_secs,
    )
    .await?;
    Ok((submitted, response))
}

async fn submit_chat_turn_json(
    graphql: &str,
    agent_did: &str,
    session_id: &str,
    behavior_id: Option<&str>,
    content: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<Value> {
    let submitted = create_agent_request(
        graphql,
        agent_did,
        content,
        Some(session_id),
        behavior_id,
        RequestSubmitOptions::default(),
    )
    .await?;
    let response =
        wait_for_terminal_response(graphql, &submitted.request_id, timeout_secs, poll_secs)
            .await
            .with_context(|| format!("waiting for AgentResponse {}", submitted.request_id))?;
    Ok(chat_turn_output(&submitted, response))
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

fn response_text_content(response: &Value) -> &str {
    response
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn chat_turn_output(submitted: &SubmittedRequest, response: Value) -> Value {
    json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
        "response": response,
    })
}

#[derive(Debug, Clone)]
struct ChatTurnProgress {
    content: String,
    reasoning: String,
    error_message: Option<String>,
    progress_seq: u64,
    status: String,
}

#[derive(Debug, Clone)]
struct ToolCallProgress {
    tool_call_key: String,
    tool_name: String,
    status: String,
    args: String,
    result: String,
}

fn chat_progress_query(request_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                session_id
                status
                content
                reasoning
                error_message
                progress_seq
                completed_at
            }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_key
                tool_name
                status
                args
                result
                started_at
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
    )
}

async fn load_existing_tool_call_keys(
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

async fn stream_turn_progress(
    graphql: &str,
    submitted: &SubmittedRequest,
    mut known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<Value> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut latest_content = String::new();
    let mut latest_reasoning = String::new();
    let mut latest_progress_seq = 0;
    let mut latest_error_message: Option<String> = None;
    let mut thinking_printed = false;

    loop {
        let query = chat_progress_query(&submitted.request_id, &submitted.session_id);
        let response = post_graphql(graphql, &query).await?;

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tool_rows
            .into_iter()
            .filter_map(|row| decode_tool_call_progress(&row))
        {
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            if previous_status.as_deref() == Some(tool.status.as_str()) {
                continue;
            }
            known_tool_calls.insert(tool.tool_call_key.clone(), tool.status.clone());
            last_progress_at = tokio::time::Instant::now();
            if previous_status.is_none() && matches!(tool.status.as_str(), "completed" | "error") {
                println!(
                    "[tool] {} {}",
                    tool.tool_name,
                    format_tool_args_preview(&tool.args)
                );
            }
            println!("{}", format_tool_progress_line(&tool));
            io::stdout().flush()?;
        }

        let response_row = response
            .pointer("/data/AgentResponse")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();
        if let Some(progress) = response_row
            .as_ref()
            .and_then(|row| decode_chat_turn_progress(row))
        {
            if progress.progress_seq > latest_progress_seq
                || progress.content != latest_content
                || progress.reasoning != latest_reasoning
                || progress.error_message != latest_error_message
            {
                last_progress_at = tokio::time::Instant::now();
            }
            if !thinking_printed
                && progress.status == "streaming"
                && progress.content.is_empty()
                && !progress.reasoning.trim().is_empty()
            {
                println!("[thinking]");
                io::stdout().flush()?;
                thinking_printed = true;
            }
            latest_progress_seq = progress.progress_seq;
            latest_error_message = progress.error_message.clone();
            latest_content = progress.content.clone();
            latest_reasoning = progress.reasoning.clone();

            if matches!(progress.status.as_str(), "complete" | "error") {
                if !progress.content.trim().is_empty() {
                    println!("{}", progress.content);
                    io::stdout().flush()?;
                }
                if progress.status == "error" {
                    if let Some(error_message) = progress
                        .error_message
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        if !progress.content.contains(error_message) {
                            println!("[agent error] {error_message}");
                            println!(
                                "[inspect] defra-agent show response {}",
                                submitted.request_id
                            );
                            io::stdout().flush()?;
                        }
                    } else {
                        println!(
                            "[inspect] defra-agent show response {}",
                            submitted.request_id
                        );
                        io::stdout().flush()?;
                    }
                }
                return Ok(response_row.unwrap_or(Value::Null));
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                submitted.request_id,
                timeout_secs,
                request_diagnostic_hint(&submitted.request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

fn decode_chat_turn_progress(row: &Value) -> Option<ChatTurnProgress> {
    Some(ChatTurnProgress {
        content: row.get("content")?.as_str()?.to_string(),
        reasoning: row
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        error_message: row
            .get("error_message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        progress_seq: row.get("progress_seq").and_then(Value::as_u64).unwrap_or(0),
        status: row.get("status")?.as_str()?.to_string(),
    })
}

fn decode_tool_call_progress(row: &Value) -> Option<ToolCallProgress> {
    Some(ToolCallProgress {
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        args: row
            .get("args")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: row
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn format_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.status.as_str() {
        "completed" => match preview_compact_text(&tool.result) {
            Some(result) => format!(
                "[tool done] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.args),
                result
            ),
            None => format!(
                "[tool done] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.args)
            ),
        },
        "error" => format!(
            "[tool error] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.args),
            preview_compact_text(&tool.result).unwrap_or_else(|| "-".to_string())
        ),
        _ => format!(
            "[tool] {} {}",
            tool.tool_name,
            format_tool_args_preview(&tool.args)
        ),
    }
}

fn format_tool_args_preview(value: &str) -> String {
    preview_compact_text(value)
        .map(|preview| format!("({preview})"))
        .unwrap_or_default()
}

fn preview_compact_text(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = if trimmed.chars().count() > 120 {
        format!("{}...", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed.to_string()
    };
    Some(preview)
}
