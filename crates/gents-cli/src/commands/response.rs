use anyhow::Result;

use crate::cli::args::{ResponseCommand, ResponseShowArgs, ResponseWaitArgs};
use crate::{
    hydrate_materialized_response_content, materialized_response_diagnostic, post_graphql,
    print_json, resolve_graphql_endpoint, resolve_request_id, response_query,
    wait_for_terminal_response, MaterializedResponsePresentation,
};

pub(crate) async fn dispatch(command: ResponseCommand) -> Result<()> {
    match command {
        ResponseCommand::Show(args) => response_show(args).await,
        ResponseCommand::Wait(args) => response_wait(args).await,
    }
}

pub(crate) async fn response_show(args: ResponseShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let query = response_query(&request_id);
    let mut response = post_graphql(&graphql, &query).await?;
    if let Some(response_row) = response.pointer_mut("/data/AgentResponse/0") {
        let presentation = hydrate_materialized_response_content(&graphql, response_row).await?;
        if matches!(
            presentation,
            MaterializedResponsePresentation::Pending | MaterializedResponsePresentation::Invalid
        ) {
            anyhow::bail!(materialized_response_diagnostic(&request_id, response_row));
        }
    }
    print_json(&response)?;
    Ok(())
}

async fn response_wait(args: ResponseWaitArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let response =
        wait_for_terminal_response(&graphql, &request_id, args.timeout_secs, args.poll_secs)
            .await?;
    print_json(&response)?;
    Ok(())
}
