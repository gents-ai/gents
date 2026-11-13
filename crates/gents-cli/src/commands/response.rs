use anyhow::Result;
use gents_store::config_client::ConfigAccess;

use crate::cli::args::{ResponseCommand, ResponseShowArgs, ResponseWaitArgs};
use crate::operation_failure::OperationFailure;
use crate::request_helpers::{request_output_envelope, request_ref, wait_for_request};
use crate::{print_json, resolve_request_id};
use gents_service::home_state::{resolve_graphql_endpoint, resolve_node_did};

pub(crate) async fn dispatch(command: ResponseCommand) -> Result<()> {
    match command {
        ResponseCommand::Show(args) => response_show(args).await,
        ResponseCommand::Wait(args) => response_wait(args).await,
    }
}

pub(crate) async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let access = ConfigAccess::Graphql(graphql);
    let request =
        gents_session::operations::request::get(&access, &request_ref(&node_did, &request_id))
            .await
            .map_err(OperationFailure::from)?;
    let output = gents_session::session::observe_request_output(&access, &request).await?;
    let envelope = request_output_envelope(&request, output)?;
    print_json(&serde_json::to_value(&envelope)?)?;
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref()).await?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let access = ConfigAccess::Graphql(graphql);
    let envelope = wait_for_request(
        &access,
        request_ref(&node_did, &request_id),
        args.timeout_secs,
        args.poll_secs,
        || async { Ok(false) },
    )
    .await?;
    print_json(&serde_json::to_value(&envelope)?)?;
    Ok(())
}
