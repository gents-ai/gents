use anyhow::{Context, Result};
use gents::ConfigAccess;
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::request_input::RequestInput;
use serde_json::json;

use crate::cli::args::{RequestCommand, RequestResendArgs, RequestSubmitArgs};
use crate::request_helpers::{
    ensure_local_request_signer, fetch_request_view, parse_valid_until_flag, RequestOutputEnvelope,
};
use crate::{
    create_agent_request, print_json, resolve_graphql_endpoint, resolve_node_did,
    resolve_request_content, resolve_request_id, wait_for_terminal_response,
    write_json_output_file, RequestSubmitOptions,
};

mod interrupt;
mod pending;
mod show;
mod show_text;
mod show_tools;
mod show_views;

use interrupt::request_interrupt;
use pending::{
    request_edit_pending, request_pending, request_remove_pending, request_reorder_pending,
};
use show::request_show;

pub(crate) async fn dispatch(command: RequestCommand) -> Result<()> {
    match command {
        RequestCommand::Submit(args) => request_submit(args).await,
        RequestCommand::Show(args) => request_show(args).await,
        RequestCommand::Interrupt(args) => request_interrupt(args).await,
        RequestCommand::Resend(args) => request_resend(args).await,
        RequestCommand::Pending(args) => request_pending(args).await,
        RequestCommand::EditPending(args) => request_edit_pending(args).await,
        RequestCommand::RemovePending(args) => request_remove_pending(args).await,
        RequestCommand::ReorderPending(args) => request_reorder_pending(args).await,
    }
}

fn wait_result(
    request_summary: serde_json::Value,
    envelope: &RequestOutputEnvelope,
) -> Result<serde_json::Value> {
    let mut object = request_summary
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request summary was not a JSON object"))?;
    object.insert(
        "output".to_string(),
        serde_json::to_value(&envelope.output)
            .context("serializing canonical request output observation")?,
    );
    Ok(serde_json::Value::Object(object))
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let content = resolve_request_content(args.content.as_deref(), args.content_file.as_deref())?;
    let valid_until = parse_valid_until_flag(args.valid_until.as_deref())?;
    ensure_local_request_signer(args.home.as_deref(), &node_did)?;
    let session_id = args
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let input = args
        .input
        .as_deref()
        .map(serde_json::from_str::<RequestInput>)
        .transpose()
        .context("--input must be canonical RequestInput JSON")?
        .unwrap_or_default();
    let input = gents::lifecycle::prepare_user_message_input(
        &ConfigAccess::Graphql(graphql.clone()),
        &node_did,
        &session_id,
        input,
        gents_protocol::request_input::QueueDelivery::Steer,
    )
    .await?;
    let submitted = create_agent_request(
        &graphql,
        &node_did,
        &content,
        Some(&session_id),
        args.agent_id.as_deref(),
        RequestSubmitOptions {
            input: Some(input),
            valid_until,
            ..Default::default()
        },
    )
    .await?;
    let request_summary = json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "node_did": submitted.node_did,
        "agent_id": submitted.agent_id,
        "input": submitted.input,
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
    .with_context(|| {
        format!(
            "waiting for request {} to reach a terminal lifecycle state",
            submitted.request_id
        )
    })?;
    let result = wait_result(request_summary, &response)?;
    print_json(&result)?;
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &result)?;
    }
    Ok(())
}

async fn request_resend(args: RequestResendArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let stale_id = resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let stale = fetch_request_view(&graphql, &stale_id).await?;
    if stale.lifecycle_state != Some(RequestLifecycleState::Dead)
        || stale.failure_reason.as_deref() != Some("Stale")
    {
        anyhow::bail!(
            "request {stale_id} is not a stale terminal (lifecycle_state={}, failure_reason={}); resend is only valid for stale-dead requests",
            stale.lifecycle_state.map_or("<missing>", RequestLifecycleState::as_str),
            stale.failure_reason.as_deref().unwrap_or("<missing>")
        );
    }
    let stale_node_did = stale
        .node_did
        .as_deref()
        .context("stale request has no node_did")?;
    let stale_content = stale
        .content
        .as_deref()
        .context("stale request has no content")?;
    let stale_doc_id = stale
        .doc_id
        .as_deref()
        .context("stale request has no _docID")?;
    let valid_until = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
    ensure_local_request_signer(args.home.as_deref(), stale_node_did)?;
    let submitted = create_agent_request(
        &graphql,
        stale_node_did,
        stale_content,
        None,
        stale.agent_id.as_deref(),
        RequestSubmitOptions {
            caused_by_source_doc_id: None,
            input: stale.input.as_ref().map(RequestInput::for_fresh_session),
            valid_until,
            retry_parent_request: Some(stale_id.clone()),
            retry_parent_request_doc_id: Some(stale_doc_id.to_string()),
            retry_root_request: stale.retry_root_request.clone(),
            retry_key: None,
        },
    )
    .await?;
    let request_summary = json!({
        "request_id": submitted.request_id,
        "session_id": submitted.session_id,
        "node_did": submitted.node_did,
        "agent_id": submitted.agent_id,
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
    .with_context(|| {
        format!(
            "waiting for request {} to reach a terminal lifecycle state",
            submitted.request_id
        )
    })?;
    let result = wait_result(request_summary, &response)?;
    print_json(&result)?;
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &result)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
