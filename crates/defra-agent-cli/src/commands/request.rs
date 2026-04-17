use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::json;

use crate::cli::args::{RequestCommand, RequestShowArgs, RequestSubmitArgs};
use crate::{
    create_agent_request, post_graphql, print_json, resolve_agent_did, resolve_graphql_endpoint,
    resolve_request_content, resolve_request_id, wait_for_terminal_response,
    write_json_output_file, RequestSubmitOptions,
};

pub(crate) async fn dispatch(command: RequestCommand) -> Result<()> {
    match command {
        RequestCommand::Submit(args) => request_submit(args).await,
        RequestCommand::Show(args) => request_show(args).await,
    }
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let content = resolve_request_content(args.content.as_deref(), args.content_file.as_deref())?;
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
            }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
    );
    let response = post_graphql(&graphql, &query).await?;
    print_json(&response)?;
    Ok(())
}
