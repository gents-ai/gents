use anyhow::{Context, Result};
use gents_protocol::operations::request::{
    DeliveryChoice, MessageSendInput, ReissueKind, RequestReissueInput,
};
use gents_protocol::request_input::RequestInput;
use gents_store::config_client::ConfigAccess;
use serde_json::json;

use crate::cli::args::{RequestCommand, RequestResendArgs, RequestSubmitArgs};
use crate::operation_failure::OperationFailure;
use crate::request_helpers::{
    client_signer, parse_valid_until_flag, request_ref, wait_for_request, RequestOutputEnvelope,
};
use crate::{print_json, resolve_request_content, resolve_request_id, write_json_output_file};
use gents_service::home_state::resolve_graphql_endpoint;
use gents_service::home_state::resolve_node_did;

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

struct Report<'a> {
    no_wait: bool,
    timeout_secs: u64,
    poll_secs: u64,
    output_file: Option<&'a std::path::Path>,
}

async fn report(
    access: &ConfigAccess,
    node_did: &str,
    request_id: &str,
    request_summary: serde_json::Value,
    how: Report<'_>,
) -> Result<()> {
    let result = if how.no_wait {
        request_summary
    } else {
        let request = request_ref(node_did, request_id);
        let envelope =
            wait_for_request(access, request, how.timeout_secs, how.poll_secs, || async {
                Ok(false)
            })
            .await?;
        wait_result(request_summary, &envelope)?
    };
    print_json(&result)?;
    if let Some(path) = how.output_file {
        write_json_output_file(path, &result)?;
    }
    Ok(())
}

async fn request_submit(args: RequestSubmitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let content = resolve_request_content(args.content.as_deref(), args.content_file.as_deref())?;
    let valid_until = parse_valid_until_flag(args.valid_until.as_deref())?;
    let input = args
        .input
        .as_deref()
        .map(serde_json::from_str::<RequestInput>)
        .transpose()
        .context("--input must be canonical RequestInput JSON")?
        .unwrap_or_default();
    let access = ConfigAccess::Graphql(graphql);
    let signer = client_signer(&access)?;
    let receipt = gents::operations::request::send(
        &access,
        &signer,
        MessageSendInput {
            node_did: node_did.clone(),
            session_id: args.session_id.clone(),
            agent_did: args.agent_did.clone(),
            content,
            input,
            delivery: DeliveryChoice::steer_when(args.steer),
            valid_until,
            ..Default::default()
        },
    )
    .await
    .map_err(OperationFailure::from)?;
    let request_summary = json!({
        "request_id": receipt.request_id,
        "session_id": receipt.session_id,
        "node_did": receipt.node_did,
        "agent_did": receipt.agent_did,
        "input": receipt.input,
    });
    report(
        &access,
        &receipt.node_did,
        &receipt.request_id,
        request_summary,
        Report {
            no_wait: args.no_wait,
            timeout_secs: args.timeout_secs,
            poll_secs: args.poll_secs,
            output_file: args.output_file.as_deref(),
        },
    )
    .await
}

async fn request_resend(args: RequestResendArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let stale_id = resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let access = ConfigAccess::Graphql(graphql);
    let signer = client_signer(&access)?;
    let receipt = gents_session::operations::request::reissue(
        &access,
        &signer,
        RequestReissueInput {
            kind: ReissueKind::Resend,
            parent: request_ref(&node_did, &stale_id),
            request_id: None,
        },
    )
    .await
    .map_err(OperationFailure::from)?;
    let request_summary = json!({
        "request_id": receipt.request_id,
        "session_id": receipt.session_id,
        "node_did": receipt.node_did,
        "agent_did": receipt.agent_did,
        "retry_parent_request": receipt.retry_parent_request,
        "retry_root_request": receipt.retry_root_request,
        "retry_count": receipt.retry_count,
        "max_retries": receipt.max_retries,
    });
    report(
        &access,
        &receipt.node_did,
        &receipt.request_id,
        request_summary,
        Report {
            no_wait: args.no_wait,
            timeout_secs: args.timeout_secs,
            poll_secs: args.poll_secs,
            output_file: args.output_file.as_deref(),
        },
    )
    .await
}

#[cfg(test)]
mod tests;
