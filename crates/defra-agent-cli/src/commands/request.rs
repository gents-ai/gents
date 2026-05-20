use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent::{graphql::escape_graphql_string, tool_call_lifecycle::CancelCause};
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::cli::args::{
    RequestCommand, RequestInterruptArgs, RequestInterruptOutputFormat, RequestResendArgs,
    RequestShowArgs, RequestSubmitArgs,
};
use crate::request_helpers::{
    fetch_request_view, is_terminal_lifecycle_state, parse_duration_suffix, parse_valid_until_flag,
};
use crate::{
    create_agent_request, post_graphql, print_json, resolve_agent_did, resolve_graphql_endpoint,
    resolve_request_content, resolve_request_id, wait_for_terminal_response,
    write_json_output_file, RequestSubmitOptions,
};

pub(crate) async fn dispatch(command: RequestCommand) -> Result<()> {
    match command {
        RequestCommand::Submit(args) => request_submit(args).await,
        RequestCommand::Show(args) => request_show(args).await,
        RequestCommand::Interrupt(args) => request_interrupt(args).await,
        RequestCommand::Resend(args) => request_resend(args).await,
    }
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let content = resolve_request_content(args.content.as_deref(), args.content_file.as_deref())?;
    let valid_until = parse_valid_until_flag(args.valid_until.as_deref())?;
    let submitted = create_agent_request(
        &graphql,
        &agent_did,
        &content,
        args.session_id.as_deref(),
        args.behavior_id.as_deref(),
        RequestSubmitOptions {
            temperature: args.temperature,
            top_p: args.top_p,
            top_k: args.top_k,
            max_tokens: args.max_tokens,
            metadata: args.metadata.clone(),
            valid_until,
            retry_parent_request: None,
            retry_root_request: None,
        },
    )
    .await?;
    let request_summary = json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
        "temperature": submitted.temperature,
        "top_p": submitted.top_p,
        "top_k": submitted.top_k,
        "max_tokens": submitted.max_tokens,
        "metadata": submitted.metadata,
    });
    if args.no_wait {
        print_json(&request_summary)?;
        if let Some(path) = args.output_file.as_deref() {
            write_json_output_file(path, &request_summary)?;
        }
        return Ok(());
    }

    let response = wait_for_terminal_response(
        &graphql,
        &submitted.request_id,
        args.timeout_secs,
        args.poll_secs,
    )
    .await
    .with_context(|| format!("waiting for AgentResponse {}", submitted.request_id))?;
    let mut output = request_summary
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request summary was not a JSON object"))?;
    output.insert("response".to_string(), response);
    let output = serde_json::Value::Object(output);
    print_json(&output)?;
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &output)?;
    }
    Ok(())
}

pub(crate) async fn request_show(args: RequestShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                status
                lifecycle_state
                backend_id
                execution_origin
                failure_reason
                retry_count
                max_retries
                temperature
                top_p
                top_k
                max_tokens
                metadata
                created_at
                claimed_at
                deadline
                valid_until
                interrupt_requested_at
            }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
    );
    let response = post_graphql(&graphql, &query).await?;
    print_json(&response)?;
    Ok(())
}

async fn request_interrupt(args: RequestInterruptArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let cancel_cause: CancelCause = args.cause.into();

    let before = fetch_interrupt_request_row(&graphql, &request_id).await?;
    let already_interrupted = request_row_string(&before, "interrupt_requested_at").is_some();
    let already_terminal = request_row_is_terminal(&before);

    if !already_interrupted && !already_terminal {
        let now = chrono::Utc::now().to_rfc3339();
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                    input: {{ interrupt_requested_at: "{now_escaped}" }}
                ) {{ _docID }}
            }}"#,
            request_id = escape_graphql_string(&request_id),
            now_escaped = escape_graphql_string(&now),
        );
        post_graphql(&graphql, &mutation).await?;
    }

    let mut row = fetch_interrupt_request_row(&graphql, &request_id).await?;
    let interrupt_landed_at = request_row_string(&row, "interrupt_requested_at");
    if !already_terminal && interrupt_landed_at.is_none() {
        anyhow::bail!("request {request_id} did not persist interrupt_requested_at");
    }

    if args.wait && !request_row_is_terminal(&row) {
        let timeout = parse_duration_suffix(&args.timeout)?;
        row = wait_for_terminal_request_state(&graphql, &request_id, timeout, row).await?;
    }

    let summary = request_interrupt_summary(
        &row,
        cancel_cause.as_str(),
        interrupt_landed_at.as_deref(),
        already_interrupted,
        already_terminal,
    );
    match args.output {
        RequestInterruptOutputFormat::Json => print_json(&summary)?,
        RequestInterruptOutputFormat::Text => print_interrupt_text(&summary)?,
    }
    Ok(())
}

async fn fetch_interrupt_request_row(graphql: &str, request_id: &str) -> Result<Value> {
    // request_id is expected to be unique; keep DESC+limit defensive for older
    // data and consistent with `request show`.
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                status
                lifecycle_state
                failure_reason
                retry_count
                max_retries
                created_at
                claimed_at
                deadline
                valid_until
                interrupt_requested_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    );
    let response = post_graphql(graphql, &query).await?;
    response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))
}

async fn wait_for_terminal_request_state(
    graphql: &str,
    request_id: &str,
    timeout: Duration,
    mut last_row: Value,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        if request_row_is_terminal(&last_row) {
            return Ok(last_row);
        }
        if Instant::now() >= deadline {
            let state = request_row_string(&last_row, "lifecycle_state")
                .unwrap_or_else(|| "<missing>".to_string());
            anyhow::bail!(
                "timed out waiting for request {request_id} to reach a terminal state after {}s (last lifecycle_state={state})",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        last_row = fetch_interrupt_request_row(graphql, request_id).await?;
    }
}

fn request_row_is_terminal(row: &Value) -> bool {
    row.get("lifecycle_state")
        .and_then(Value::as_str)
        .is_some_and(is_terminal_lifecycle_state)
}

fn request_row_string(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn request_interrupt_summary(
    row: &Value,
    cause: &str,
    interrupt_landed_at: Option<&str>,
    already_interrupted: bool,
    already_terminal: bool,
) -> Value {
    let lifecycle_state = request_row_string(row, "lifecycle_state").unwrap_or_default();
    json!({
        "request_id": request_row_string(row, "request_id").unwrap_or_default(),
        "agent_did": request_row_string(row, "agent_did"),
        "behavior_id": request_row_string(row, "behavior_id"),
        "session_id": request_row_string(row, "session_id"),
        "status": request_row_string(row, "status"),
        "lifecycle_state": lifecycle_state,
        "failure_reason": request_row_string(row, "failure_reason"),
        "interrupt_requested_at": request_row_string(row, "interrupt_requested_at"),
        "interrupt_landed_at": interrupt_landed_at,
        "cause": cause,
        "already_interrupted": already_interrupted,
        "already_terminal": already_terminal,
        "terminal": is_terminal_lifecycle_state(&lifecycle_state),
        "created_at": request_row_string(row, "created_at"),
        "claimed_at": request_row_string(row, "claimed_at"),
        "deadline": request_row_string(row, "deadline"),
        "valid_until": request_row_string(row, "valid_until"),
    })
}

fn print_interrupt_text(summary: &Value) -> Result<()> {
    let text = |key: &str| {
        summary
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("-")
            .to_string()
    };
    println!("request_id: {}", text("request_id"));
    println!("state: {}", text("lifecycle_state"));
    println!("status: {}", text("status"));
    println!("interrupt_landed_at: {}", text("interrupt_landed_at"));
    println!("cause: {}", text("cause"));
    println!(
        "already_interrupted: {}",
        summary
            .get("already_interrupted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "already_terminal: {}",
        summary
            .get("already_terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "terminal: {}",
        summary
            .get("terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if summary
        .get("already_terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && summary
            .get("interrupt_landed_at")
            .is_none_or(Value::is_null)
    {
        println!("note: request was already terminal; interrupt was not latched");
    }
    if let Some(reason) = summary
        .get("failure_reason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        println!("failure_reason: {reason}");
    }
    Ok(())
}

async fn request_resend(args: RequestResendArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let stale_id = resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let stale = fetch_request_view(&graphql, &stale_id).await?;
    if stale.lifecycle_state != "dead" || stale.failure_reason != "Stale" {
        anyhow::bail!(
            "request {stale_id} is not a stale terminal (lifecycle_state={}, failure_reason={}); resend is only valid for stale-dead requests",
            stale.lifecycle_state,
            stale.failure_reason
        );
    }
    let valid_until = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
    let submitted = create_agent_request(
        &graphql,
        &stale.agent_did,
        &stale.content,
        None,
        stale.behavior_id.as_deref(),
        RequestSubmitOptions {
            // Preserve sampling overrides + metadata from the original row.
            // Dropping these would silently change model behavior on retry.
            temperature: stale.temperature,
            top_p: stale.top_p,
            top_k: stale.top_k,
            max_tokens: stale.max_tokens,
            metadata: stale.metadata.clone(),
            valid_until,
            retry_parent_request: Some(stale_id.clone()),
            retry_root_request: stale.retry_root_request.clone(),
        },
    )
    .await?;
    let request_summary = json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "agent_did": submitted.agent_did,
        "behavior_id": submitted.behavior_id,
        "retry_parent_request": stale_id,
        "retry_root_request": stale.retry_root_request,
    });
    if args.no_wait {
        print_json(&request_summary)?;
        if let Some(path) = args.output_file.as_deref() {
            write_json_output_file(path, &request_summary)?;
        }
        return Ok(());
    }
    let response = wait_for_terminal_response(
        &graphql,
        &submitted.request_id,
        args.timeout_secs,
        args.poll_secs,
    )
    .await
    .with_context(|| format!("waiting for AgentResponse {}", submitted.request_id))?;
    let mut output = request_summary
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request summary was not a JSON object"))?;
    output.insert("response".to_string(), response);
    let output = serde_json::Value::Object(output);
    print_json(&output)?;
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &output)?;
    }
    Ok(())
}
