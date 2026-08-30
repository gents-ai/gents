mod streaming;

use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::cli::args::ChatArgs;
use crate::cli::output_format::OutputFormat;
use crate::request_helpers::ensure_local_request_signer;
use crate::{
    create_agent_request, print_json, require_non_empty, resolve_home_dir,
    wait_for_terminal_response, write_json_output_file, RequestSubmitOptions, SubmittedRequest,
    DEFAULT_HTTP_PORT,
};

use streaming::{load_existing_tool_call_keys, stream_turn_progress};

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

    if let Some(message) = resolve_chat_message(&args.message, args.message_file.as_deref())? {
        match args
            .output_format
            .ensure_supported("chat", &[OutputFormat::Text, OutputFormat::Json])?
        {
            OutputFormat::Text => {
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
            OutputFormat::Json => {
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

pub(crate) async fn submit_chat_turn(
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
