use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::json;

use crate::cli::args::{
    RequestCommand, RequestInterruptArgs, RequestResendArgs, RequestShowArgs, RequestSubmitArgs,
};
use crate::request_helpers::{fetch_request_view, parse_valid_until_flag};
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
    // Combined existence + latch-status check. We distinguish "no row" from
    // "row with empty field" so that interrupting a bogus request id reports
    // an error instead of silently succeeding with a no-op mutation.
    // Idempotent: if the field is already set, leave the original latch in place
    // so the runtime observes a single canonical interrupt timestamp.
    let existing_query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                request_id
                interrupt_requested_at
            }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
    );
    let existing = post_graphql(&graphql, &existing_query).await?;
    let existing_row = existing.pointer("/data/AgentRequest/0");
    let Some(existing_row) = existing_row else {
        anyhow::bail!("request {request_id} not found");
    };
    let already = existing_row
        .get("interrupt_requested_at")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    if let Some(existing_at) = already {
        let summary = json!({
            "request_id": request_id,
            "interrupt_requested_at": existing_at,
            "already_interrupted": true,
        });
        print_json(&summary)?;
        return Ok(());
    }

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
    let summary = json!({
        "request_id": request_id,
        "interrupt_requested_at": now,
        "already_interrupted": false,
    });
    print_json(&summary)?;
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
