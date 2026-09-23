use anyhow::Result;

use crate::cli::args::{ResponseCommand, ResponseShowArgs, ResponseWaitArgs};
use crate::request_helpers::{
    fetch_request_view, observe_canonical_request_output, request_output_envelope,
    wait_for_terminal_response,
};
use crate::{print_json, resolve_graphql_endpoint, resolve_request_id};

pub(crate) async fn dispatch(command: ResponseCommand) -> Result<()> {
    match command {
        ResponseCommand::Show(args) => response_show(args).await,
        ResponseCommand::Wait(args) => response_wait(args).await,
    }
}

/// Typed canonical request/output envelope. The signed admission input stays on
/// the request row and is never rewritten into response-shaped JSON; `Published`
/// observations are nonterminal and stay distinct from `TerminalMessage`.
pub(crate) async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let request = fetch_request_view(&graphql, &request_id).await?;
    let output = observe_canonical_request_output(&graphql, &request).await?;
    let envelope = request_output_envelope(&request, output)?;
    print_json(&serde_json::to_value(&envelope)?)?;
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let envelope =
        wait_for_terminal_response(&graphql, &request_id, args.timeout_secs, args.poll_secs)
            .await?;
    print_json(&serde_json::to_value(&envelope)?)?;
    Ok(())
}
